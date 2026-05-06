use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, warn};

use crate::{Bundle, SubmitResult, SubmitTier, Submitter};

/// Blink (formerly Merkle) private mempool submission.
/// Supports both BSC and Base. Transactions are held privately and forwarded
/// to all major builders. Free with API key from blinklabs.xyz.
/// Docs: https://docs.merkle.io/private-pool/wallets/send-transactions-via-rpc
pub struct BlinkSubmitter {
    endpoint: String,
    chain_label: &'static str,
    client: reqwest::Client,
}

impl BlinkSubmitter {
    pub fn new(endpoint: &str, chain_label: &'static str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            chain_label,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Submitter for BlinkSubmitter {
    fn venue_name(&self) -> &'static str {
        match self.chain_label {
            "BSC" => "Blink_BSC",
            "Base" => "Blink_Base",
            _ => "Blink",
        }
    }

    fn tier(&self) -> SubmitTier {
        SubmitTier::AlwaysOn
    }

    async fn submit(&self, bundle: &Bundle) -> Result<SubmitResult> {
        if bundle.signed_txs.is_empty() {
            return Ok(SubmitResult {
                venue: self.venue_name(),
                success: false,
                bundle_hash: None,
                error: Some("No transactions".into()),
            });
        }

        let raw_tx = format!("0x{}", hex::encode(&bundle.signed_txs[0]));

        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendRawTransaction",
            "params": [raw_tx]
        });

        let resp = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;

        if let Some(error) = body.get("error") {
            let msg = error.to_string();
            warn!(venue = self.chain_label, error = %msg, "Blink submission failed");
            return Ok(SubmitResult {
                venue: self.venue_name(),
                success: false,
                bundle_hash: None,
                error: Some(msg),
            });
        }

        let tx_hash = body
            .get("result")
            .and_then(|r| r.as_str())
            .map(String::from);

        debug!(venue = self.chain_label, tx_hash = ?tx_hash, "Blink tx submitted");

        Ok(SubmitResult {
            venue: self.venue_name(),
            success: true,
            bundle_hash: tx_hash,
            error: None,
        })
    }
}
