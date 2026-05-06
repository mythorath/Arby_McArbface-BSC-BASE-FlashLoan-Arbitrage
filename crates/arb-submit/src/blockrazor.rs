use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, warn};

use crate::{Bundle, SubmitResult, SubmitTier, Submitter};

/// BlockRazor bundle submission for BSC.
/// BlockRazor builds ~40% of BSC blocks — the second most important venue.
/// Endpoint: https://bsc.blockrazor.xyz
/// Method: eth_sendMevBundle
/// Docs: https://blockrazor.gitbook.io/blockrazor/transaction-submission/bundle/bsc/project-builder
pub struct BlockRazorSubmitter {
    endpoint: String,
    client: reqwest::Client,
}

impl BlockRazorSubmitter {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Submitter for BlockRazorSubmitter {
    fn venue_name(&self) -> &'static str {
        "BlockRazor"
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
            "method": "eth_sendMevBundle",
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
            warn!(venue = "BlockRazor", error = %msg, "Bundle rejected");
            return Ok(SubmitResult {
                venue: self.venue_name(),
                success: false,
                bundle_hash: None,
                error: Some(msg),
            });
        }

        let result_field = body.get("result");
        let has_result = result_field.is_some() && !result_field.unwrap().is_null();
        let bundle_hash = result_field
            .and_then(|r| r.as_str().or_else(|| r.get("bundleHash").and_then(|h| h.as_str())))
            .map(String::from);

        debug!(venue = "BlockRazor", bundle_hash = ?bundle_hash, success = has_result, "Bundle submitted");

        Ok(SubmitResult {
            venue: self.venue_name(),
            success: has_result,
            bundle_hash,
            error: if has_result { None } else { Some("no result in response".into()) },
        })
    }
}
