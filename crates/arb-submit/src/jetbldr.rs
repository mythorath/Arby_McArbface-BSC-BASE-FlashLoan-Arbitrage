use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, warn};

use crate::{Bundle, SubmitResult, SubmitTier, Submitter};

/// JetBldr bundle submission for BSC.
/// JetBldr builds ~1.6% of BSC blocks.
/// Default endpoint: https://rpc.bsc-virginia.jetbldr.xyz
/// Regional alternatives: bsc-eu, bsc-tokyo, bsc-dublin
/// Method: eth_sendBundle (Flashbots-compatible)
/// Docs: https://jetbldr.xyz/api_bsc/eth_sendbundle/
pub struct JetBldrSubmitter {
    endpoint: String,
    client: reqwest::Client,
}

impl JetBldrSubmitter {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Submitter for JetBldrSubmitter {
    fn venue_name(&self) -> &'static str {
        "JetBldr"
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
            warn!(venue = "JetBldr", error = %msg, "Bundle rejected");
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
            .and_then(|r| r.get("bundleHash").and_then(|h| h.as_str()).or_else(|| r.as_str()))
            .map(String::from);

        debug!(venue = "JetBldr", bundle_hash = ?bundle_hash, success = has_result, "Bundle submitted");

        Ok(SubmitResult {
            venue: self.venue_name(),
            success: has_result,
            bundle_hash,
            error: if has_result { None } else { Some("no result in response".into()) },
        })
    }
}
