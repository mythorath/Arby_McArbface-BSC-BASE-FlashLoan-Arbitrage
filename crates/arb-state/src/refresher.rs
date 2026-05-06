use std::sync::Arc;
use std::time::Instant;

use alloy::sol;
use alloy_primitives::{Address, U256};
use anyhow::Result;
use tracing::{debug, warn};

use arb_core::types::*;
use arb_rpc::Endpoint;

use crate::pool_store::PoolStore;

sol! {
    #[sol(rpc)]
    interface IStateReader {
        struct V2State {
            address pool;
            address token0;
            address token1;
            uint112 reserve0;
            uint112 reserve1;
            uint32 fee;
        }

        struct V3State {
            address pool;
            address token0;
            address token1;
            uint160 sqrtPriceX96;
            int24 tick;
            uint128 liquidity;
            uint24 fee;
            bool unlocked;
        }

        struct AlgebraState {
            address pool;
            address token0;
            address token1;
            uint160 sqrtPriceX96;
            int24 tick;
            uint128 liquidity;
            uint16 feeZto;
            uint16 feeOtz;
            bool unlocked;
        }

        struct AeroV2State {
            address pool;
            address token0;
            address token1;
            uint256 reserve0;
            uint256 reserve1;
            bool stable;
            uint256 decimals0;
            uint256 decimals1;
            uint32 fee;
        }

        struct PcsStableState {
            address pool;
            address token0;
            address token1;
            uint256 balance0;
            uint256 balance1;
            uint256 A;
            uint256 fee;
            uint256 adminFee;
        }

        struct DodoV2State {
            address pool;
            address baseToken;
            address quoteToken;
            uint256 baseReserve;
            uint256 quoteReserve;
            uint256 baseTarget;
            uint256 quoteTarget;
            uint8 rState;
            uint256 k;
            uint256 lpFeeRate;
            uint256 mtFeeRate;
        }

        struct WombatState {
            address pool;
            address token0;
            address token1;
            uint256 cash0;
            uint256 cash1;
            uint256 liability0;
            uint256 liability1;
            uint256 ampFactor;
            uint256 haircutRate;
        }

        function readV2(address[] calldata pools) external view returns (V2State[] memory);
        function readV3(address[] calldata pools) external view returns (V3State[] memory);
        function readAlgebra(address[] calldata pools) external view returns (AlgebraState[] memory);
        function readAeroV2(address[] calldata pools) external view returns (AeroV2State[] memory);
        function readPcsStable(address[] calldata pools) external view returns (PcsStableState[] memory);
        function readDodoV2(address[] calldata pools) external view returns (DodoV2State[] memory);
        function readWombat(address[] calldata pools, address[] calldata token0s, address[] calldata token1s) external view returns (WombatState[] memory);
    }
}

pub struct PoolConfig {
    pub address: Address,
    pub protocol: Protocol,
    pub fee_bps: u32,
    pub token0: Option<Address>,
    pub token1: Option<Address>,
}

/// Curated factory-address → default fee table. Used when the on-chain reader
/// returns fee=0 (i.e., the pool contract doesn't expose its fee).
fn default_fee_for_factory(factory: Address, chain_id: u64) -> Option<u32> {
    match chain_id {
        56 => {
            let factory_str = format!("{:?}", factory).to_lowercase();
            match factory_str.as_str() {
                // PancakeSwap V2
                s if s.contains("ca143ce32fe78f1f7019d7d551a6402fc5350c73") => Some(25),
                // BiSwap
                s if s.contains("858e3312ed3a876947ea49d572a7c42de08af7ee") => Some(10),
                // MDEX
                s if s.contains("3cd1c46068daea5ebb0d3f55f6915b10648062b8") => Some(30),
                // ApeSwap
                s if s.contains("0841bd0b734e4f5853f0dd8d7ea989891dbdcfb5") => Some(20),
                _ => None,
            }
        }
        8453 => {
            let factory_str = format!("{:?}", factory).to_lowercase();
            match factory_str.as_str() {
                // BaseSwap V2
                s if s.contains("fda619b6d20975be80a10332cd39b9a4b0faa8bb") => Some(25),
                // SushiSwap V2
                s if s.contains("71524b4f93c58fcbf659783284e38825f0622859") => Some(30),
                _ => None,
            }
        }
        _ => None,
    }
}

fn partition_pools(configs: &[PoolConfig]) -> (Vec<Address>, Vec<Address>, Vec<Address>, Vec<Address>,
                                                Vec<Address>, Vec<Address>, Vec<Address>) {
    let mut v2 = Vec::new();
    let mut v3 = Vec::new();
    let mut algebra = Vec::new();
    let mut aero = Vec::new();
    let mut pcs_stable = Vec::new();
    let mut wombat = Vec::new();
    let mut dodo = Vec::new();

    for pc in configs {
        match pc.protocol {
            Protocol::UniswapV2 => v2.push(pc.address),
            Protocol::UniswapV3 | Protocol::AerodromeSlipstream => v3.push(pc.address),
            Protocol::UniswapV4 => {}
            Protocol::Algebra => algebra.push(pc.address),
            Protocol::AerodromeV2 => aero.push(pc.address),
            Protocol::PancakeStable => pcs_stable.push(pc.address),
            Protocol::Wombat => wombat.push(pc.address),
            Protocol::DodoV2 => dodo.push(pc.address),
        }
    }

    (v2, v3, algebra, aero, pcs_stable, wombat, dodo)
}

pub struct StateRefresher {
    endpoint: Arc<Endpoint>,
    state_reader_addr: Address,
    pool_configs: Vec<PoolConfig>,
    chain_id: u64,
}

impl StateRefresher {
    const CHUNK_SIZE: usize = 50;

    pub fn new(
        endpoint: Arc<Endpoint>,
        state_reader_addr: Address,
        pool_configs: Vec<PoolConfig>,
        chain_id: u64,
    ) -> Self {
        Self {
            endpoint,
            state_reader_addr,
            pool_configs,
            chain_id,
        }
    }

    pub async fn refresh(&self, store: &PoolStore) -> Result<(usize, std::time::Duration)> {
        let start = Instant::now();
        let mut updated = 0;

        let (v2_addrs, v3_addrs, algebra_addrs, aero_addrs,
             pcs_stable_addrs, wombat_addrs, dodo_addrs) = self.partition_by_type();

        let provider = self.endpoint.provider();
        let reader = IStateReader::new(self.state_reader_addr, provider);

        let v2_chunks: Vec<_> = v2_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let v3_chunks: Vec<_> = v3_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let algebra_chunks: Vec<_> = algebra_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let aero_chunks: Vec<_> = aero_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let pcs_chunks: Vec<_> = pcs_stable_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let dodo_chunks: Vec<_> = dodo_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();

        let wombat_data: Vec<(Address, Address, Address)> = wombat_addrs.iter()
            .filter_map(|addr| {
                let cfg = self.pool_configs.iter().find(|c| c.address == *addr)?;
                Some((*addr, cfg.token0?, cfg.token1?))
            })
            .collect();
        let wombat_pools: Vec<Address> = wombat_data.iter().map(|d| d.0).collect();
        let wombat_t0s: Vec<Address> = wombat_data.iter().map(|d| d.1).collect();
        let wombat_t1s: Vec<Address> = wombat_data.iter().map(|d| d.2).collect();

        let (v2_results, v3_results, algebra_results, aero_results,
             pcs_results, dodo_results, wombat_results) = tokio::join!(
            async {
                let mut all = Vec::new();
                for chunk in &v2_chunks {
                    match reader.readV2(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "V2 chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &v3_chunks {
                    match reader.readV3(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "V3 chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &algebra_chunks {
                    match reader.readAlgebra(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "Algebra chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &aero_chunks {
                    match reader.readAeroV2(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "AeroV2 chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &pcs_chunks {
                    match reader.readPcsStable(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "PCS Stable chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &dodo_chunks {
                    match reader.readDodoV2(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "DODO chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                if wombat_pools.is_empty() {
                    return Vec::new();
                }
                match reader.readWombat(wombat_pools.clone(), wombat_t0s.clone(), wombat_t1s.clone()).call().await {
                    Ok(states) => states,
                    Err(e) => {
                        warn!("Wombat read failed: {e}");
                        Vec::new()
                    }
                }
            },
        );

        for s in &v2_results {
            let onchain_fee = s.fee as u32;
            let fee_bps = if onchain_fee > 0 {
                onchain_fee
            } else {
                self.fee_for_pool(&s.pool)
            };
            store.update(
                s.pool,
                PoolState::V2(V2PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    reserve0: U256::from(s.reserve0),
                    reserve1: U256::from(s.reserve1),
                    fee_bps,
                }),
            );
            updated += 1;
        }

        for s in &v3_results {
            if s.sqrtPriceX96.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::V3(V3PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    sqrt_price_x96: U256::from(s.sqrtPriceX96),
                    tick: s.tick.as_i32(),
                    liquidity: s.liquidity,
                    fee: s.fee.to::<u32>(),
                    fee_otz: None,
                }),
            );
            updated += 1;
        }

        for s in &algebra_results {
            if s.sqrtPriceX96.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::V3(V3PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    sqrt_price_x96: U256::from(s.sqrtPriceX96),
                    tick: s.tick.as_i32(),
                    liquidity: s.liquidity,
                    fee: s.feeZto as u32,
                    fee_otz: Some(s.feeOtz as u32),
                }),
            );
            updated += 1;
        }

        for s in &aero_results {
            let fee_bps = if s.fee > 0 {
                s.fee
            } else {
                self.fee_for_pool(&s.pool)
            };
            store.update(
                s.pool,
                PoolState::AeroV2(AeroV2PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    reserve0: s.reserve0,
                    reserve1: s.reserve1,
                    stable: s.stable,
                    fee_bps,
                    decimals0: s.decimals0,
                    decimals1: s.decimals1,
                }),
            );
            updated += 1;
        }

        for s in &pcs_results {
            if s.balance0.is_zero() && s.balance1.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::Curve(CurvePoolState {
                    address: s.pool,
                    tokens: vec![s.token0, s.token1],
                    balances: vec![s.balance0, s.balance1],
                    amp: s.A,
                    fee: s.fee,
                }),
            );
            updated += 1;
        }

        for s in &dodo_results {
            if s.baseReserve.is_zero() && s.quoteReserve.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::Dodo(DodoPoolState {
                    address: s.pool,
                    base_token: s.baseToken,
                    quote_token: s.quoteToken,
                    base_reserve: s.baseReserve,
                    quote_reserve: s.quoteReserve,
                    base_target: s.baseTarget,
                    quote_target: s.quoteTarget,
                    r_state: s.rState,
                    k: s.k,
                    lp_fee_rate: s.lpFeeRate,
                    mt_fee_rate: s.mtFeeRate,
                }),
            );
            updated += 1;
        }

        for s in &wombat_results {
            if s.cash0.is_zero() && s.cash1.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::Wombat(WombatPoolState {
                    address: s.pool,
                    token_in: s.token0,
                    token_out: s.token1,
                    cash_in: s.cash0,
                    cash_out: s.cash1,
                    liability_in: s.liability0,
                    liability_out: s.liability1,
                    amp: s.ampFactor,
                    haircut_rate: s.haircutRate,
                }),
            );
            updated += 1;
        }

        let block = self.endpoint.block_number().await.unwrap_or(0);
        store.set_block(block);

        let elapsed = start.elapsed();
        debug!(
            updated,
            elapsed_ms = elapsed.as_millis(),
            block,
            "State refresh completed"
        );

        Ok((updated, elapsed))
    }

    fn partition_by_type(&self) -> (Vec<Address>, Vec<Address>, Vec<Address>, Vec<Address>,
                                     Vec<Address>, Vec<Address>, Vec<Address>) {
        partition_pools(&self.pool_configs)
    }

    fn fee_for_pool(&self, pool: &Address) -> u32 {
        self.pool_configs
            .iter()
            .find(|pc| pc.address == *pool)
            .map(|pc| pc.fee_bps)
            .unwrap_or(30)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::with_last_byte(b)
    }

    #[test]
    fn test_default_fee_pancakeswap_v2_bsc() {
        let factory: Address = "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 56), Some(25));
    }

    #[test]
    fn test_default_fee_biswap_bsc() {
        let factory: Address = "0x858E3312ed3A876947EA49d572A7C42DE08af7EE".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 56), Some(10));
    }

    #[test]
    fn test_default_fee_mdex_bsc() {
        let factory: Address = "0x3CD1C46068dAEa5Ebb0d3f55F6915B10648062b8".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 56), Some(30));
    }

    #[test]
    fn test_default_fee_apeswap_bsc() {
        let factory: Address = "0x0841BD0B734E4F5853f0dD8d7Ea989891DBdcFb5".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 56), Some(20));
    }

    #[test]
    fn test_default_fee_baseswap_base() {
        let factory: Address = "0xFDa619b6d20975be80A10332cD39b9a4b0FAa8BB".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 8453), Some(25));
    }

    #[test]
    fn test_default_fee_sushiswap_base() {
        let factory: Address = "0x71524B4f93c58fcbF659783284E38825f0622859".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 8453), Some(30));
    }

    #[test]
    fn test_default_fee_unknown_factory() {
        assert_eq!(default_fee_for_factory(addr(99), 56), None);
        assert_eq!(default_fee_for_factory(addr(99), 8453), None);
    }

    #[test]
    fn test_default_fee_unknown_chain() {
        let factory: Address = "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".parse().unwrap();
        assert_eq!(default_fee_for_factory(factory, 1), None);
    }

    #[test]
    fn test_partition_routes_correctly() {
        let configs = vec![
            PoolConfig { address: addr(1), protocol: Protocol::UniswapV2, fee_bps: 25, token0: None, token1: None },
            PoolConfig { address: addr(2), protocol: Protocol::UniswapV3, fee_bps: 0, token0: None, token1: None },
            PoolConfig { address: addr(3), protocol: Protocol::Algebra, fee_bps: 0, token0: None, token1: None },
            PoolConfig { address: addr(4), protocol: Protocol::AerodromeV2, fee_bps: 30, token0: None, token1: None },
            PoolConfig { address: addr(5), protocol: Protocol::PancakeStable, fee_bps: 0, token0: None, token1: None },
            PoolConfig { address: addr(6), protocol: Protocol::Wombat, fee_bps: 0, token0: Some(addr(10)), token1: Some(addr(11)) },
            PoolConfig { address: addr(7), protocol: Protocol::DodoV2, fee_bps: 0, token0: None, token1: None },
            PoolConfig { address: addr(8), protocol: Protocol::UniswapV4, fee_bps: 0, token0: None, token1: None },
            PoolConfig { address: addr(9), protocol: Protocol::AerodromeSlipstream, fee_bps: 0, token0: None, token1: None },
        ];
        let (v2, v3, algebra, aero, pcs, wombat, dodo) = partition_pools(&configs);
        assert_eq!(v2, vec![addr(1)]);
        assert_eq!(v3, vec![addr(2), addr(9)], "V3 + Slipstream");
        assert_eq!(algebra, vec![addr(3)]);
        assert_eq!(aero, vec![addr(4)]);
        assert_eq!(pcs, vec![addr(5)]);
        assert_eq!(wombat, vec![addr(6)]);
        assert_eq!(dodo, vec![addr(7)]);
    }

    #[test]
    fn test_v4_excluded_from_all_partitions() {
        let configs = vec![
            PoolConfig { address: addr(1), protocol: Protocol::UniswapV4, fee_bps: 0, token0: None, token1: None },
        ];
        let (v2, v3, algebra, aero, pcs, wombat, dodo) = partition_pools(&configs);
        assert!(v2.is_empty() && v3.is_empty() && algebra.is_empty() && aero.is_empty()
                && pcs.is_empty() && wombat.is_empty() && dodo.is_empty());
    }
}
