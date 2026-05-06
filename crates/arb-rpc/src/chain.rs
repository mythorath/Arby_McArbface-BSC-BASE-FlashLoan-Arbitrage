use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub name: String,
    /// Free/cheap Growth endpoint — used for ALL reads (state refresh, block number, nonce, gas price)
    pub rpc_https: String,
    pub rpc_wss: String,
    /// Expensive Trader/Warp endpoint — used ONLY for eth_sendRawTransaction.
    /// On Chainstack Trader nodes each call costs ~$0.15, so never use this for reads.
    pub trader_rpc: Option<String>,
    pub arb_contract: String,
    pub state_reader: String,
    pub block_time_ms: u64,
    pub scan_budget_ms: u64,
}
