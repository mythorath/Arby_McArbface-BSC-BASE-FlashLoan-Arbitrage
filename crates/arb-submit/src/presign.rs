use std::collections::HashMap;

use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use anyhow::Result;

use arb_core::types::Protocol;
use arb_paths::PathTemplate;
use arb_rpc::Endpoint;

use crate::builder::{PoolKey, SwapInstruction, executeV4ArbitrageCall};
use crate::Bundle;

fn gas_for_protocol(protocol: Protocol) -> u64 {
    match protocol {
        Protocol::UniswapV2 => 120_000,
        Protocol::UniswapV3 => 220_000,
        Protocol::AerodromeV2 => 140_000,
        Protocol::AerodromeSlipstream => 230_000,
        Protocol::Algebra => 230_000,
        Protocol::PancakeStable => 250_000,
        Protocol::Wombat => 260_000,
        Protocol::DodoV2 => 250_000,
        Protocol::UniswapV4 => 300_000,
    }
}

struct CalldataTemplate {
    asset: Address,
    swap_instructions: Vec<SwapInstruction>,
    gas_limit: u64,
}

pub struct PresignPool {
    templates: HashMap<u32, CalldataTemplate>,
    chain_id: u64,
}

impl PresignPool {
    pub fn new(paths: &[PathTemplate], chain_id: u64) -> Self {
        let mut templates = HashMap::new();
        for path in paths {
            let has_v4_hop = path.hops.iter().any(|h| h.protocol == Protocol::UniswapV4);
            if has_v4_hop {
                continue;
            }

            let swap_instructions: Vec<SwapInstruction> = path
                .hops
                .iter()
                .map(|hop| SwapInstruction {
                    protocol: hop.protocol.to_contract_enum(chain_id),
                    pool: hop.pool,
                    poolKey: PoolKey {
                        currency0: Address::ZERO,
                        currency1: Address::ZERO,
                        fee: alloy_primitives::Uint::from(0u32),
                        tickSpacing: alloy_primitives::Signed::ZERO,
                        hooks: Address::ZERO,
                    },
                    tokenIn: hop.token_in,
                    tokenOut: hop.token_out,
                    minOut: U256::ZERO,
                })
                .collect();

            let gas_limit: u64 = 80_000 + path.hops.iter()
                .map(|h| gas_for_protocol(h.protocol))
                .sum::<u64>();

            templates.insert(path.id, CalldataTemplate {
                asset: path.flash_token,
                swap_instructions,
                gas_limit,
            });
        }

        Self { templates, chain_id }
    }

    pub async fn build_fast(
        &self,
        path_id: u32,
        flash_amount: U256,
        endpoint: &Endpoint,
        arb_contract: Address,
        signer: &PrivateKeySigner,
        target_block: u64,
    ) -> Result<Bundle> {
        let tpl = self.templates.get(&path_id)
            .ok_or_else(|| anyhow::anyhow!("No presign template for path {path_id}"))?;

        let deadline = U256::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 120,
        );

        let calldata = executeV4ArbitrageCall {
            asset: tpl.asset,
            amount: flash_amount,
            swapInstructions: tpl.swap_instructions.clone(),
            deadline,
        }
        .abi_encode();

        let nonce = endpoint.get_nonce(signer.address()).await?;

        let buf = if self.chain_id == 8453 {
            let gas_price = endpoint.gas_price().await?;
            let max_priority_fee: u128 = 1_000_000_000; // 1 gwei
            let max_fee = gas_price.saturating_mul(2).saturating_add(max_priority_fee);

            let tx = alloy::consensus::TxEip1559 {
                chain_id: self.chain_id,
                nonce,
                gas_limit: tpl.gas_limit,
                max_fee_per_gas: max_fee,
                max_priority_fee_per_gas: max_priority_fee,
                to: arb_contract.into(),
                value: U256::ZERO,
                input: Bytes::from(calldata).into(),
                access_list: Default::default(),
            };
            let sig = signer
                .sign_hash(&alloy::consensus::SignableTransaction::signature_hash(&tx))
                .await?;
            let envelope = alloy::consensus::TxEnvelope::Eip1559(
                alloy::consensus::Signed::new_unchecked(tx, sig, Default::default()),
            );
            let mut b = Vec::new();
            alloy::eips::eip2718::Encodable2718::encode_2718(&envelope, &mut b);
            b
        } else {
            let gas_price = endpoint.gas_price().await?;
            let tx = alloy::consensus::TxLegacy {
                chain_id: Some(self.chain_id),
                nonce,
                gas_price,
                gas_limit: tpl.gas_limit,
                to: arb_contract.into(),
                value: U256::ZERO,
                input: Bytes::from(calldata).into(),
            };
            let sig = signer
                .sign_hash(&alloy::consensus::SignableTransaction::signature_hash(&tx))
                .await?;
            let envelope = alloy::consensus::TxEnvelope::Legacy(
                alloy::consensus::Signed::new_unchecked(tx, sig, Default::default()),
            );
            let mut b = Vec::new();
            alloy::eips::eip2718::Encodable2718::encode_2718(&envelope, &mut b);
            b
        };

        Ok(Bundle {
            signed_txs: vec![buf],
            target_block,
            chain_id: self.chain_id,
            backrun_tx: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_paths::{HopTemplate, PathTemplate};
    use alloy_primitives::address;

    #[test]
    fn test_gas_for_protocol_values() {
        assert_eq!(gas_for_protocol(Protocol::UniswapV2), 120_000);
        assert_eq!(gas_for_protocol(Protocol::UniswapV3), 220_000);
        assert_eq!(gas_for_protocol(Protocol::AerodromeV2), 140_000);
        assert_eq!(gas_for_protocol(Protocol::AerodromeSlipstream), 230_000);
        assert_eq!(gas_for_protocol(Protocol::Algebra), 230_000);
        assert_eq!(gas_for_protocol(Protocol::PancakeStable), 250_000);
        assert_eq!(gas_for_protocol(Protocol::Wombat), 260_000);
        assert_eq!(gas_for_protocol(Protocol::DodoV2), 250_000);
    }

    #[test]
    fn test_presign_pool_gas_limit_v2_only() {
        let paths = vec![PathTemplate {
            id: 0,
            flash_token: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            flash_amount: U256::from(1000u32),
            hops: vec![
                HopTemplate {
                    protocol: Protocol::UniswapV2,
                    pool: address!("1111111111111111111111111111111111111111"),
                    token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                    token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                },
                HopTemplate {
                    protocol: Protocol::UniswapV2,
                    pool: address!("2222222222222222222222222222222222222222"),
                    token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                    token_out: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                },
            ],
        }];
        let pool = PresignPool::new(&paths, 56);
        let tpl = pool.templates.get(&0).unwrap();
        assert_eq!(tpl.gas_limit, 80_000 + 120_000 * 2);
    }

    #[test]
    fn test_presign_pool_gas_limit_mixed() {
        let paths = vec![PathTemplate {
            id: 0,
            flash_token: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            flash_amount: U256::from(1000u32),
            hops: vec![
                HopTemplate {
                    protocol: Protocol::UniswapV2,
                    pool: address!("1111111111111111111111111111111111111111"),
                    token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                    token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                },
                HopTemplate {
                    protocol: Protocol::UniswapV3,
                    pool: address!("2222222222222222222222222222222222222222"),
                    token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                    token_out: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                },
            ],
        }];
        let pool = PresignPool::new(&paths, 56);
        let tpl = pool.templates.get(&0).unwrap();
        assert_eq!(tpl.gas_limit, 80_000 + 120_000 + 220_000);
    }

    #[test]
    fn test_presign_pool_base_uses_eip1559() {
        let paths = vec![PathTemplate {
            id: 0,
            flash_token: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            flash_amount: U256::from(1000u32),
            hops: vec![HopTemplate {
                protocol: Protocol::UniswapV2,
                pool: address!("1111111111111111111111111111111111111111"),
                token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            }],
        }];
        let pool = PresignPool::new(&paths, 8453);
        assert_eq!(pool.chain_id, 8453);
        assert!(pool.templates.contains_key(&0));
    }

    #[test]
    fn test_presign_pool_bsc_uses_legacy() {
        let paths = vec![PathTemplate {
            id: 0,
            flash_token: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            flash_amount: U256::from(1000u32),
            hops: vec![HopTemplate {
                protocol: Protocol::UniswapV2,
                pool: address!("1111111111111111111111111111111111111111"),
                token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            }],
        }];
        let pool = PresignPool::new(&paths, 56);
        assert_eq!(pool.chain_id, 56);
        assert!(pool.templates.contains_key(&0));
    }

    #[test]
    fn test_gas_limit_three_hop_mixed() {
        let paths = vec![PathTemplate {
            id: 0,
            flash_token: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            flash_amount: U256::from(1000u32),
            hops: vec![
                HopTemplate {
                    protocol: Protocol::UniswapV2,
                    pool: address!("1111111111111111111111111111111111111111"),
                    token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                    token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                },
                HopTemplate {
                    protocol: Protocol::Algebra,
                    pool: address!("2222222222222222222222222222222222222222"),
                    token_in: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                    token_out: address!("cccccccccccccccccccccccccccccccccccccccc"),
                },
                HopTemplate {
                    protocol: Protocol::AerodromeV2,
                    pool: address!("3333333333333333333333333333333333333333"),
                    token_in: address!("cccccccccccccccccccccccccccccccccccccccc"),
                    token_out: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                },
            ],
        }];
        let pool = PresignPool::new(&paths, 8453);
        let tpl = pool.templates.get(&0).unwrap();
        assert_eq!(tpl.gas_limit, 80_000 + 120_000 + 230_000 + 140_000);
    }

    #[test]
    fn test_presign_pool_skips_v4() {
        let paths = vec![PathTemplate {
            id: 0,
            flash_token: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            flash_amount: U256::from(1000u32),
            hops: vec![HopTemplate {
                protocol: Protocol::UniswapV4,
                pool: address!("1111111111111111111111111111111111111111"),
                token_in: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token_out: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            }],
        }];
        let pool = PresignPool::new(&paths, 56);
        assert!(pool.templates.is_empty());
    }
}
