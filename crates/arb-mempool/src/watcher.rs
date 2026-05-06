use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::consensus::Transaction as TxTrait;
use alloy::network::TransactionResponse;
use alloy_primitives::Address;
use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::decoder::{DecodedSwap, TxDecoder};

#[derive(Debug, Clone)]
pub struct PendingSwap {
    pub tx_hash: alloy_primitives::B256,
    pub from: Address,
    pub to: Address,
    pub value: alloy_primitives::U256,
    pub decoded: DecodedSwap,
    pub raw_input: Vec<u8>,
}

pub struct MempoolWatcher {
    wss_url: String,
    chain_id: u64,
    decoder: TxDecoder,
}

impl MempoolWatcher {
    pub fn new(wss_url: &str, chain_id: u64) -> Self {
        Self {
            wss_url: wss_url.to_string(),
            chain_id,
            decoder: TxDecoder::new(),
        }
    }

    pub async fn start(self, tx: mpsc::Sender<PendingSwap>) -> Result<()> {
        info!(
            url = %self.wss_url,
            chain_id = self.chain_id,
            "Connecting to mempool WSS"
        );

        let ws = WsConnect::new(&self.wss_url);
        let provider = ProviderBuilder::new().connect_ws(ws).await?;

        // Base (8453): Flashblocks gives 200ms-resolution tx visibility ~1.8s before
        // block seal. Chainstack supports newFlashblockTransactions but it only returns
        // tx hashes, not full txs. We still use full pending tx subscription for the
        // decoder pipeline and get the latency benefit from the Flashblocks-aware node.
        if self.chain_id == 8453 {
            info!("Base chain: using Flashblocks-aware pending tx subscription (200ms resolution)");
        } else {
            info!("BSC chain: using standard pending tx subscription");
        }

        let sub = provider.subscribe_full_pending_transactions().await?;
        let mut stream = sub.into_stream();

        while let Some(pending_tx) = stream.next().await {
            let to_addr = match pending_tx.to() {
                Some(addr) => addr,
                None => continue,
            };

            let input = pending_tx.input().to_vec();
            if input.len() < 4 {
                continue;
            }

            if let Some(decoded) = self.decoder.decode(to_addr, &input) {
                let swap = PendingSwap {
                    tx_hash: pending_tx.tx_hash(),
                    from: pending_tx.from(),
                    to: to_addr,
                    value: pending_tx.value(),
                    decoded,
                    raw_input: input,
                };

                if tx.send(swap).await.is_err() {
                    warn!("Mempool channel closed, stopping watcher");
                    break;
                }
            }
        }

        Ok(())
    }
}
