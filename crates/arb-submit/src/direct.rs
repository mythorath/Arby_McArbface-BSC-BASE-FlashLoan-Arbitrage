use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, warn};

use arb_rpc::Endpoint;

use crate::{Bundle, SubmitResult, SubmitTier, Submitter};

/// Direct eth_sendRawTransaction via the free Growth RPC endpoint.
/// Always-on fallback — no bundling, no MEV protection, but free and reliable.
pub struct DirectSubmitter {
    endpoint: Arc<Endpoint>,
}

impl DirectSubmitter {
    pub fn new(endpoint: Arc<Endpoint>) -> Self {
        Self { endpoint }
    }
}

#[async_trait]
impl Submitter for DirectSubmitter {
    fn venue_name(&self) -> &'static str {
        "Direct_RPC"
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
                error: Some("No transactions in bundle".into()),
            });
        }

        let raw_tx = alloy_primitives::Bytes::from(bundle.signed_txs[0].clone());
        // send_raw_tx_free() routes through the Growth read endpoint, never the Trader/Warp endpoint.
        match self.endpoint.send_raw_tx_free(raw_tx).await {
            Ok(tx_hash) => {
                debug!(venue = "Direct", tx_hash = %tx_hash, "Transaction submitted");
                Ok(SubmitResult {
                    venue: self.venue_name(),
                    success: true,
                    bundle_hash: Some(format!("{tx_hash}")),
                    error: None,
                })
            }
            Err(e) => {
                warn!(venue = "Direct", error = %e, "Transaction submission failed");
                Ok(SubmitResult {
                    venue: self.venue_name(),
                    success: false,
                    bundle_hash: None,
                    error: Some(e.to_string()),
                })
            }
        }
    }
}
