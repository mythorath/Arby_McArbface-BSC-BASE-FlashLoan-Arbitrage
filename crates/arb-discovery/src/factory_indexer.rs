use std::collections::HashSet;

use alloy_primitives::Address;
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

use crate::feeds::FeedCandidate;

/// Factory event configuration for a DEX.
#[derive(Debug, Clone)]
pub struct FactoryEntry {
    pub factory_address: Address,
    pub protocol: String,
    pub exchange_name: String,
    /// keccak256 of the PairCreated / PoolCreated event signature
    pub event_topic: [u8; 32],
}

/// Known BSC factory addresses and their PairCreated/PoolCreated topics.
pub fn bsc_factories() -> Vec<FactoryEntry> {
    vec![
        FactoryEntry {
            factory_address: "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".parse().unwrap(),
            protocol: "v2".to_string(),
            exchange_name: "PancakeSwap V2".to_string(),
            // PairCreated(address,address,address,uint256)
            event_topic: hex_literal("0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"),
        },
        FactoryEntry {
            factory_address: "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865".parse().unwrap(),
            protocol: "v3".to_string(),
            exchange_name: "PancakeSwap V3".to_string(),
            // PoolCreated(address,address,uint24,int24,address)
            event_topic: hex_literal("0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"),
        },
        FactoryEntry {
            factory_address: "0xdB1d10011AD0Ff90774D0C6Bb92e5C5c8b4461F7".parse().unwrap(),
            protocol: "v3".to_string(),
            exchange_name: "Uniswap V3".to_string(),
            event_topic: hex_literal("0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"),
        },
        FactoryEntry {
            factory_address: "0x858e3312ed3a876947ea49d572a7c42de08af7ee".parse().unwrap(),
            protocol: "v2".to_string(),
            exchange_name: "BiSwap".to_string(),
            event_topic: hex_literal("0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"),
        },
    ]
}

/// Known Base factory addresses.
pub fn base_factories() -> Vec<FactoryEntry> {
    vec![
        FactoryEntry {
            factory_address: "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6".parse().unwrap(),
            protocol: "v2".to_string(),
            exchange_name: "BaseSwap V2".to_string(),
            event_topic: hex_literal("0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"),
        },
        FactoryEntry {
            factory_address: "0x33128a8fC17869897dcE68Ed026d694621f6FDfD".parse().unwrap(),
            protocol: "v3".to_string(),
            exchange_name: "Uniswap V3".to_string(),
            event_topic: hex_literal("0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"),
        },
        FactoryEntry {
            factory_address: "0x420DD381b31aEf6683db6B902084cB0FFECe40Da".parse().unwrap(),
            protocol: "aero_v2".to_string(),
            exchange_name: "Aerodrome V2".to_string(),
            event_topic: hex_literal("0xc4805696c2d1a5cadf0c3de220e3e5837c43fd06dd36e17d1e018cf2e8654c1b"),
        },
        FactoryEntry {
            factory_address: "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A".parse().unwrap(),
            protocol: "aero_slipstream".to_string(),
            exchange_name: "Aerodrome Slipstream".to_string(),
            event_topic: hex_literal("0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"),
        },
    ]
}

fn hex_literal(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str.trim_start_matches("0x")).unwrap();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes[..32]);
    arr
}

/// Placeholder for the factory event listener.
/// In production this would subscribe to WSS logs and decode PairCreated events.
/// For now, this is a stub that can be expanded later.
pub async fn run_factory_indexer(
    _chain_name: &str,
    _wss_url: &str,
    _factories: Vec<FactoryEntry>,
    _tx: mpsc::Sender<FeedCandidate>,
    _seen: HashSet<String>,
) -> Result<()> {
    info!("Factory indexer stub — not yet subscribed to WSS events");
    // In a real implementation:
    // 1. Connect to WSS
    // 2. Subscribe to logs for all factory addresses
    // 3. Decode PairCreated(token0, token1, pair, ...)
    // 4. Check if either token is already in our universe
    // 5. If not, send a FeedCandidate with source="factory"
    //
    // For now, we rely on CMC feeds as the primary discovery source.
    // This function will block forever (the caller spawns it as a task).
    std::future::pending::<()>().await;
    Ok(())
}
