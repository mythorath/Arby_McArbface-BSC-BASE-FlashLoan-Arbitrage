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

/// Cached calldata template for a path — everything except the flash amount is static.
struct CalldataTemplate {
    asset: Address,
    swap_instructions: Vec<SwapInstruction>,
}

/// Pre-encoded calldata cache keyed by path_id.
/// Avoids re-encoding the SwapInstruction array on every submit.
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

            templates.insert(path.id, CalldataTemplate {
                asset: path.flash_token,
                swap_instructions,
            });
        }

        Self { templates, chain_id }
    }

    /// Build a bundle using the cached calldata template.
    /// Skips the per-hop SwapInstruction construction; only encodes the final call
    /// with the given flash amount.
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
        let gas_price = endpoint.gas_price().await?;

        let tx = alloy::consensus::TxLegacy {
            chain_id: Some(self.chain_id),
            nonce,
            gas_price,
            gas_limit: 200_000 + 150_000 * tpl.swap_instructions.len() as u64,
            to: arb_contract.into(),
            value: U256::ZERO,
            input: Bytes::from(calldata).into(),
        };

        let sig = signer
            .sign_hash(&alloy::consensus::SignableTransaction::signature_hash(&tx))
            .await?;

        let signed = alloy::consensus::TxEnvelope::Legacy(
            alloy::consensus::Signed::new_unchecked(tx, sig, Default::default()),
        );

        let mut buf = Vec::new();
        alloy::eips::eip2718::Encodable2718::encode_2718(&signed, &mut buf);

        Ok(Bundle {
            signed_txs: vec![buf],
            target_block,
            chain_id: self.chain_id,
            backrun_tx: None,
        })
    }
}
