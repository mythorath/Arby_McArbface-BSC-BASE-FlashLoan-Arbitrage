use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, warn};

use crate::{Bundle, SubmitResult, SubmitTier, Submitter};

/// NodeReal Builder bundle submission for BSC.
/// NodeReal builds ~0.7% of BSC blocks.
/// Endpoint: https://bsc-mainnet-builder.nodereal.io
/// Method: eth_sendBundle
/// Docs: https://docs.nodereal.io/docs/send-bundle
pub struct NodeRealSubmitter {
    endpoint: String,
    client: reqwest::Client,
}

impl NodeRealSubmitter {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Submitter for NodeRealSubmitter {
    fn venue_name(&self) -> &'static str {
        "NodeReal"
    }

    fn tier(&self) -> SubmitTier {
        SubmitTier::AlwaysOn
    }

    async fn submit(&self, bundle: &Bundle) -> Result<SubmitResult> {
        let txs: Vec<String> = bundle
            .signed_txs
            .iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": [{
                "txs": txs,
                "maxBlockNumber": bundle.target_block,
                "revertingTxHashes": [],
            }]
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
            warn!(venue = "NodeReal", error = %msg, "Bundle rejected");
            return Ok(SubmitResult {
                venue: self.venue_name(),
                success: false,
                bundle_hash: None,
                error: Some(msg),
            });
        }

        let result_field = body.get("result");
        let has_result = result_field.is_some() && !result_field.unwrap().is_null();
        let bundle_hash = result_field.and_then(|r| r.as_str()).map(String::from);

        debug!(venue = "NodeReal", bundle_hash = ?bundle_hash, success = has_result, "Bundle submitted");

        Ok(SubmitResult {
            venue: self.venue_name(),
            success: has_result,
            bundle_hash,
            error: if has_result { None } else { Some("no result in response".into()) },
        })
    }
}
