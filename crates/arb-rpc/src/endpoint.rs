use std::sync::Mutex;
use std::time::{Duration, Instant};

use alloy::providers::{Provider, ProviderBuilder};
use alloy_primitives::{Address, Bytes, U256};
use anyhow::Result;
use tracing::{debug, info, warn};

type HttpProvider = alloy::providers::fillers::FillProvider<
    alloy::providers::fillers::JoinFill<
        alloy::providers::Identity,
        alloy::providers::fillers::JoinFill<
            alloy::providers::fillers::GasFiller,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::BlobGasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::NonceFiller,
                    alloy::providers::fillers::ChainIdFiller,
                >,
            >,
        >,
    >,
    alloy::providers::RootProvider,
>;

pub struct Endpoint {
    http_url: String,
    wss_url: String,
    /// Free Growth endpoint for ALL reads
    read_provider: HttpProvider,
    /// Optional Trader/Warp endpoint ONLY for eth_sendRawTransaction.
    /// Falls back to read_provider if not set.
    trader_provider: Option<HttpProvider>,
    #[allow(dead_code)]
    trader_url: Option<String>,
    chain_id: u64,
    /// Cached nonce: None = cold start (needs RPC fetch).
    /// Incremented locally after each submit to avoid rapid-fire nonce collisions.
    nonce_cache: Mutex<Option<u64>>,
}

impl Endpoint {
    pub async fn new(
        http_url: &str,
        wss_url: &str,
        trader_url: Option<&str>,
        chain_id: u64,
    ) -> Result<Self> {
        let read_provider = ProviderBuilder::new()
            .connect_http(http_url.parse()?);

        let detected_chain = read_provider.get_chain_id().await?;
        if detected_chain != chain_id {
            anyhow::bail!(
                "Chain ID mismatch on read endpoint: expected {chain_id}, got {detected_chain}"
            );
        }
        info!(chain_id, endpoint = http_url, "Read endpoint connected");

        let trader_provider = if let Some(turl) = trader_url {
            if turl.is_empty() {
                info!("No trader endpoint configured, will use read endpoint for tx submission");
                None
            } else {
                let tp = ProviderBuilder::new()
                    .connect_http(turl.parse()?);
                let trader_chain = tp.get_chain_id().await?;
                if trader_chain != chain_id {
                    anyhow::bail!(
                        "Chain ID mismatch on trader endpoint: expected {chain_id}, got {trader_chain}"
                    );
                }
                info!(chain_id, endpoint = turl, "Trader endpoint connected (tx submission only)");
                Some(tp)
            }
        } else {
            info!("No trader endpoint configured, will use read endpoint for tx submission");
            None
        };

        Ok(Self {
            http_url: http_url.to_string(),
            wss_url: wss_url.to_string(),
            read_provider,
            trader_provider,
            trader_url: trader_url.map(String::from),
            chain_id,
            nonce_cache: Mutex::new(None),
        })
    }

    pub fn provider(&self) -> &HttpProvider {
        &self.read_provider
    }

    pub fn http_url(&self) -> &str {
        &self.http_url
    }

    pub fn wss_url(&self) -> &str {
        &self.wss_url
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn has_trader_endpoint(&self) -> bool {
        self.trader_provider.is_some()
    }

    /// eth_call against the FREE read endpoint. Used for state refresh, simulations, etc.
    pub async fn eth_call_timed(
        &self,
        to: Address,
        data: Bytes,
    ) -> Result<(Bytes, Duration)> {
        let start = Instant::now();

        let tx = alloy::rpc::types::TransactionRequest::default()
            .to(to)
            .input(data.into());

        let result = self.read_provider.call(tx).await?;
        let elapsed = start.elapsed();

        debug!(
            chain_id = self.chain_id,
            latency_ms = elapsed.as_millis(),
            "eth_call completed (read endpoint)"
        );

        Ok((result, elapsed))
    }

    /// Block number from the FREE read endpoint.
    pub async fn block_number(&self) -> Result<u64> {
        Ok(self.read_provider.get_block_number().await?)
    }

    /// Nonce: returns cached value if warm, otherwise fetches from chain.
    /// Call `bump_nonce()` after each successful submit to keep the cache ahead.
    pub async fn get_nonce(&self, address: Address) -> Result<u64> {
        {
            let guard = self.nonce_cache.lock().unwrap();
            if let Some(n) = *guard {
                return Ok(n);
            }
        }
        let chain_nonce = self.read_provider.get_transaction_count(address).await?;
        let mut guard = self.nonce_cache.lock().unwrap();
        *guard = Some(chain_nonce);
        debug!(chain_id = self.chain_id, nonce = chain_nonce, "Nonce fetched from chain (cold start)");
        Ok(chain_nonce)
    }

    /// Increment the cached nonce after a successful submit.
    pub fn bump_nonce(&self) {
        let mut guard = self.nonce_cache.lock().unwrap();
        if let Some(ref mut n) = *guard {
            *n += 1;
        }
    }

    /// Force-refresh nonce from chain (e.g. after a "nonce too low" error).
    pub async fn refresh_nonce(&self, address: Address) -> Result<u64> {
        let chain_nonce = self.read_provider.get_transaction_count(address).await?;
        let mut guard = self.nonce_cache.lock().unwrap();
        let old = *guard;
        *guard = Some(chain_nonce);
        warn!(
            chain_id = self.chain_id,
            old_nonce = ?old,
            new_nonce = chain_nonce,
            "Nonce refreshed from chain"
        );
        Ok(chain_nonce)
    }

    /// Gas price from the FREE read endpoint.
    pub async fn gas_price(&self) -> Result<u128> {
        Ok(self.read_provider.get_gas_price().await?)
    }

    /// Get transaction receipt from the FREE read endpoint.
    pub async fn get_receipt(&self, tx_hash: alloy_primitives::B256) -> Result<Option<alloy::rpc::types::TransactionReceipt>> {
        Ok(self.read_provider.get_transaction_receipt(tx_hash).await?)
    }

    /// Get native balance from the FREE read endpoint.
    pub async fn get_balance(&self, address: Address) -> Result<U256> {
        Ok(self.read_provider.get_balance(address).await?)
    }

    /// Send raw transaction via the TRADER endpoint (costs $0.15/call on Chainstack Trader).
    /// Falls back to read endpoint only if no trader is configured.
    ///
    /// IMPORTANT: Only call this from `WarpSubmitter`. Every call to this function
    /// that reaches a real Trader endpoint costs $0.15 — this is tracked in metrics.
    /// Use `send_raw_tx_free()` for all other submission venues.
    pub async fn send_raw_tx(&self, raw_tx: Bytes) -> Result<alloy_primitives::B256> {
        let provider = self.trader_provider.as_ref().unwrap_or(&self.read_provider);
        let label = if self.trader_provider.is_some() { "trader" } else { "read" };

        let start = Instant::now();
        let pending = provider.send_raw_transaction(&raw_tx).await?;
        let elapsed = start.elapsed();

        if self.trader_provider.is_some() {
            warn!(
                chain_id = self.chain_id,
                latency_ms = elapsed.as_millis(),
                tx_hash = %pending.tx_hash(),
                "PAID Warp tx sent ($0.15)"
            );
        } else {
            info!(
                chain_id = self.chain_id,
                endpoint = label,
                latency_ms = elapsed.as_millis(),
                tx_hash = %pending.tx_hash(),
                "Transaction sent"
            );
        }

        Ok(*pending.tx_hash())
    }

    /// Send raw transaction via the FREE read endpoint ONLY.
    /// Never touches the Trader endpoint regardless of configuration.
    /// Use this for DirectSubmitter and any other "free" venue.
    pub async fn send_raw_tx_free(&self, raw_tx: Bytes) -> Result<alloy_primitives::B256> {
        let start = Instant::now();
        let pending = self.read_provider.send_raw_transaction(&raw_tx).await?;
        let elapsed = start.elapsed();

        info!(
            chain_id = self.chain_id,
            endpoint = "read",
            latency_ms = elapsed.as_millis(),
            tx_hash = %pending.tx_hash(),
            "Transaction sent (free endpoint)"
        );

        Ok(*pending.tx_hash())
    }
}
