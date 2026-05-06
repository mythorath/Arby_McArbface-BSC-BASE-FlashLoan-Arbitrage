use arb_core::types::Protocol;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use anyhow::Result;

use arb_paths::PathTemplate;
use arb_rpc::Endpoint;

use crate::Bundle;

alloy::sol! {
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }

    struct SwapInstruction {
        uint8 protocol;
        address pool;
        PoolKey poolKey;
        address tokenIn;
        address tokenOut;
        uint256 minOut;
    }

    function executeV4Arbitrage(
        address asset,
        uint256 amount,
        SwapInstruction[] calldata swapInstructions,
        uint256 deadline
    ) external;
}

pub async fn build_bundle(
    path: &PathTemplate,
    endpoint: &Endpoint,
    arb_contract: Address,
    signer: &PrivateKeySigner,
    target_block: u64,
    chain_id: u64,
) -> Result<Bundle> {
    // V4 is only used as the flash loan source (unlock/take), never as a swap hop.
    // The PoolKey fields (currency0/1, fee, tickSpacing, hooks) are not populated,
    // so any V4 swap hop would revert on-chain.
    for hop in &path.hops {
        if hop.protocol == Protocol::UniswapV4 {
            anyhow::bail!("V4 swap hops are not supported — V4 is only used as flash loan source");
        }
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

    let deadline = U256::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120,
    );

    let calldata = executeV4ArbitrageCall {
        asset: path.flash_token,
        amount: path.flash_amount,
        swapInstructions: swap_instructions,
        deadline,
    }
    .abi_encode();

    let nonce = endpoint.get_nonce(signer.address()).await?;
    let gas_price = endpoint.gas_price().await?;

    let tx = alloy::consensus::TxLegacy {
        chain_id: Some(chain_id),
        nonce,
        gas_price,
        gas_limit: 500_000,
        to: arb_contract.into(),
        value: U256::ZERO,
        input: Bytes::from(calldata).into(),
    };

    let sig = signer
        .sign_hash(&alloy::consensus::SignableTransaction::signature_hash(&tx))
        .await?;

    let signed = alloy::consensus::TxEnvelope::Legacy(alloy::consensus::Signed::new_unchecked(
        tx,
        sig,
        Default::default(),
    ));

    let mut buf = Vec::new();
    alloy::eips::eip2718::Encodable2718::encode_2718(&signed, &mut buf);

    Ok(Bundle {
        signed_txs: vec![buf],
        target_block,
        chain_id,
        backrun_tx: None,
    })
}
