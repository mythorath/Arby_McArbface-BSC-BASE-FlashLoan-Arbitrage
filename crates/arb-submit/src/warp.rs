use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, warn};

use arb_rpc::Endpoint;

use crate::{Bundle, SubmitResult, SubmitTier, Submitter};

/// Chainstack Trader/Warp submission — sends eth_sendRawTransaction via the
/// paid Trader endpoint. On BSC this routes through bloXroute BDN under the hood.
/// Each call costs ~$0.15, so only fire on high-EV trades.
pub struct WarpSubmitter {
    endpoint: Arc<Endpoint>,
}

impl WarpSubmitter {
    pub fn new(endpoint: Arc<Endpoint>) -> Self {
        Self { endpoint }
    }
}

#[async_trait]
impl Submitter for WarpSubmitter {
    fn venue_name(&self) -> &'static str {
        "Warp_Trader"
    }

    fn tier(&self) -> SubmitTier {
        SubmitTier::HighEvOnly
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

        let raw_tx = alloy_primitives::Bytes::from(bundle.signed_txs[0].clone());
        match self.endpoint.send_raw_tx(raw_tx).await {
            Ok(tx_hash) => {
                debug!(venue = "Warp", tx_hash = %tx_hash, "Warp tx submitted");
                Ok(SubmitResult {
                    venue: self.venue_name(),
                    success: true,
                    bundle_hash: Some(format!("{tx_hash}")),
                    error: None,
                })
            }
            Err(e) => {
                warn!(venue = "Warp", error = %e, "Warp tx submission failed");
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
