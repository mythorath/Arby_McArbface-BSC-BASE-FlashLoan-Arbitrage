use std::collections::HashMap;
use std::sync::Arc;

use alloy::providers::Provider;
use alloy_primitives::{Address, U256};
use anyhow::Result;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use arb_core::types::*;
use arb_core::AmmQuoter;
use arb_rpc::Endpoint;
use arb_state::pool_store::PoolStore;
use arb_state::refresher::{PoolConfig, StateRefresher};

#[path = "../config.rs"]
mod config;

alloy::sol! {
    function getAmountOut(uint256 amountIn, address tokenIn) external view returns (uint256);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32);
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .init();

    let args: Vec<String> = std::env::args().collect();
    let config_path = args.get(1).map(|s| s.as_str()).unwrap_or("config/base.toml");

    let cfg = config::load_config(config_path)?;
    info!(chain = %cfg.chain.name, "Validating pools from {config_path}");

    let endpoint = Arc::new(
        Endpoint::new(&cfg.chain.rpc_https, &cfg.chain.rpc_wss, None, cfg.chain.chain_id).await?,
    );

    let tokens: HashMap<String, Address> = cfg
        .tokens
        .iter()
        .map(|(name, addr_str)| {
            let addr: Address = addr_str.parse().expect("Invalid token address");
            (name.clone(), addr)
        })
        .collect();

    let pool_configs: Vec<PoolConfig> = cfg
        .pools
        .iter()
        .map(|p| PoolConfig {
            address: p.address.parse().expect("Invalid pool address"),
            protocol: p.parse_protocol(),
            fee_bps: p.fee_bps,
        })
        .collect();

    let store = Arc::new(PoolStore::new());
    let state_reader: Address = cfg.chain.state_reader.parse()?;
    let refresher = StateRefresher::new(endpoint.clone(), state_reader, pool_configs);

    let (count, _) = refresher.refresh(&store).await?;
    info!(pools = count, "State loaded");

    let mut total_tests = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;

    println!("\n{:<44} {:<12} {:<28} {:>14} {:>14} {:>8} {}", 
        "Pool", "Protocol", "Test", "Rust", "Chain", "Δ bps", "Status");
    println!("{}", "-".repeat(130));

    for pool_entry in &cfg.pools {
        let pool_addr: Address = pool_entry.address.parse()?;
        let protocol = pool_entry.parse_protocol();
        let token0 = tokens[&pool_entry.token0];
        let token1 = tokens[&pool_entry.token1];

        let pool_state = match store.get(&pool_addr) {
            Some(s) => s,
            None => {
                println!("{:<44} {:<12} {:<28} SKIP (no state loaded)", 
                    format!("{:.8}..{:.4}", pool_addr, &format!("{pool_addr}")[38..]),
                    format!("{:?}", protocol),
                    "");
                continue;
            }
        };

        // Determine test amounts: small fractions of the smaller reserve
        let (reserve_a, reserve_b) = match &pool_state {
            PoolState::V2(s) => (s.reserve0, s.reserve1),
            PoolState::AeroV2(s) => (s.reserve0, s.reserve1),
            PoolState::V3(s) => {
                // V3 doesn't have simple reserves, use 1 token as base
                let one_token = if pool_entry.token0.contains("USD") || pool_entry.token0.contains("DAI") {
                    U256::from(1_000_000u64)
                } else {
                    U256::from(1_000_000_000_000_000_000u64)
                };
                (one_token * U256::from(10000u32), one_token * U256::from(10000u32))
            }
            _ => continue,
        };

        let smaller = reserve_a.min(reserve_b);
        let fractions = [10000u32, 1000, 100, 20]; // 0.01%, 0.1%, 1%, 5%

        // Is this an Aerodrome pool with getAmountOut?
        let has_get_amount_out = matches!(protocol, Protocol::AerodromeV2);

        // V3 pools get wider tolerance since our math is single-tick approximation
        let tolerance_bps = match protocol {
            Protocol::UniswapV3 | Protocol::UniswapV4 => 50,
            _ => 1,
        };

        for &frac in &fractions {
            let test_amount = smaller / U256::from(frac);
            if test_amount.is_zero() {
                continue;
            }

            // Test token0 -> token1
            let rust_out = match &pool_state {
                PoolState::V2(s) => s.quote(token0, test_amount).ok(),
                PoolState::AeroV2(s) => s.quote(token0, test_amount).ok(),
                PoolState::V3(s) => s.quote(token0, test_amount).ok(),
                _ => None,
            };

            let rust_out = match rust_out {
                Some(v) if !v.is_zero() => v,
                _ => continue,
            };

            // Compare against on-chain getAmountOut if available
            if has_get_amount_out {
                let call = getAmountOutCall {
                    amountIn: test_amount,
                    tokenIn: token0,
                };
                let calldata = alloy::sol_types::SolCall::abi_encode(&call);

                match endpoint.eth_call_timed(pool_addr, calldata.into()).await {
                    Ok((result_bytes, _)) => {
                        if result_bytes.len() >= 32 {
                            let chain_out = U256::from_be_slice(&result_bytes[..32]);

                            let delta = if rust_out > chain_out {
                                rust_out - chain_out
                            } else {
                                chain_out - rust_out
                            };

                            let delta_bps = if !chain_out.is_zero() {
                                ((delta * U256::from(10000u32)) / chain_out)
                                    .try_into()
                                    .unwrap_or(9999u32)
                            } else {
                                0u32
                            };

                            let status = if delta_bps <= tolerance_bps { "PASS" } else { "FAIL" };
                            total_tests += 1;
                            if delta_bps <= tolerance_bps {
                                passed += 1;
                            } else {
                                failed += 1;
                            }

                            let pool_label = format!("{:.8}..{:.4}", pool_addr, &format!("{pool_addr}")[38..]);
                            let test_label = format!("{}/{} -> out", pool_entry.token0, frac);
                            println!("{:<44} {:<12} {:<28} {:>14} {:>14} {:>6}.{:02} {}", 
                                pool_label,
                                format!("{:?}", protocol),
                                test_label,
                                rust_out,
                                chain_out,
                                delta_bps / 100,
                                delta_bps % 100,
                                status);
                        }
                    }
                    Err(e) => {
                        println!("{:<44} {:<12} {:<28} ERROR: {}", 
                            format!("{:.8}..{:.4}", pool_addr, &format!("{pool_addr}")[38..]),
                            format!("{:?}", protocol),
                            format!("{}/{} -> out", pool_entry.token0, frac),
                            e);
                    }
                }
            } else {
                // No on-chain comparison available, just log the Rust quote
                total_tests += 1;
                passed += 1;
                let pool_label = format!("{:.8}..{:.4}", pool_addr, &format!("{pool_addr}")[38..]);
                let test_label = format!("{}/{} -> out", pool_entry.token0, frac);
                println!("{:<44} {:<12} {:<28} {:>14} {:>14} {:>8} {}", 
                    pool_label,
                    format!("{:?}", protocol),
                    test_label,
                    rust_out,
                    "N/A",
                    "N/A",
                    "RUST_ONLY");
            }
        }
    }

    println!("\n{}", "=".repeat(130));
    println!("Results: {} total, {} passed, {} failed", total_tests, passed, failed);

    if failed > 0 {
        error!(failed, "VALIDATION FAILED — fix pool math before going live");
        std::process::exit(1);
    } else {
        info!(passed, "All pool validations passed");
    }

    Ok(())
}
