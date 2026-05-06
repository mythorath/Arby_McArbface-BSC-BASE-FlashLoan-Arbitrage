use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use arb_discovery::cmc::CmcClient;
use arb_discovery::enrich::{self, EnrichConfig};
use arb_discovery::feeds::{self, FeedCandidate, FeedConfig, PlatformConfig};
use arb_discovery::filter::FilterConfig;
use arb_discovery::store::DiscoveryStore;

use std::collections::HashSet;

fn load_majors_for_chain(chain: &str) -> HashSet<String> {
    let mut majors = HashSet::new();
    match chain {
        "BSC" | "bsc" => {
            majors.insert("0x55d398326f99059fF775485246999027B3197955".to_lowercase()); // USDT
            majors.insert("0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d".to_lowercase()); // USDC
            majors.insert("0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c".to_lowercase()); // WBNB
            majors.insert("0x2170Ed0880ac9A755fd29B2688956BD959F933F8".to_lowercase()); // ETH
            majors.insert("0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c".to_lowercase()); // BTCB
            majors.insert("0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56".to_lowercase()); // BUSD
        }
        "Base" | "base" => {
            majors.insert("0x4200000000000000000000000000000000000006".to_lowercase()); // WETH
            majors.insert("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_lowercase()); // USDC
            majors.insert("0xd9AAec86b65D86f6a7b5B1b0c42fFA531710b6Aa".to_lowercase()); // USDbC
            majors.insert("0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb".to_lowercase()); // DAI
            majors.insert("0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".to_lowercase()); // cbBTC
        }
        _ => {}
    }
    majors
}

async fn run_chain(
    chain: &str,
    client: Arc<CmcClient>,
    store: Arc<DiscoveryStore>,
    use_standard_api: bool,
) -> Result<()> {
    let platform = match chain {
        "BSC" | "bsc" => PlatformConfig::bsc(),
        "Base" | "base" => PlatformConfig::base(),
        _ => anyhow::bail!("Unknown chain: {chain}"),
    };

    let majors = load_majors_for_chain(chain);

    let filter_cfg = FilterConfig {
        min_liquidity_usd: 25_000.0,
        min_volume_usd_24h: 50_000.0,
        max_buy_tax_bps: 100,
        max_sell_tax_bps: 100,
        require_audit_passed: true,
        require_social: true,
        major_tokens: majors,
    };

    let enrich_cfg = EnrichConfig {
        max_buy_tax_bps: 100,
        max_sell_tax_bps: 100,
        platform_name: platform.platform_name.clone(),
        chain_name: platform.chain_name.clone(),
    };

    let feed_cfg = FeedConfig {
        use_standard_api,
        ..FeedConfig::default()
    };
    let (tx, mut rx) = mpsc::channel::<FeedCandidate>(500);

    let feed_client = client.clone();
    let feed_platform = platform.clone();
    tokio::spawn(async move {
        if let Err(e) = feeds::run_feeds(feed_client, feed_platform, feed_cfg, tx).await {
            error!(error = %e, "Feed loop exited with error");
        }
    });

    info!(chain = %chain, mode = if use_standard_api { "standard" } else { "dex" },
        "Discovery enrichment loop starting");

    while let Some(candidate) = rx.recv().await {
        let chain_lower = chain.to_lowercase();

        if use_standard_api {
            // Standard API mode: we have the token from listings but no pool/security data.
            // Save the token directly — the runner will discover pools on-chain.
            let token = match candidate.token {
                Some(t) => t,
                None => continue,
            };

            if token.volume_24h_usd < filter_cfg.min_volume_usd_24h {
                continue;
            }

            let mut token_uni = store.load_tokens(&chain_lower).unwrap_or_default();
            let existing = token_uni.tokens.iter_mut()
                .find(|t| t.address.to_lowercase() == token.address.to_lowercase());
            if let Some(existing) = existing {
                existing.last_seen = token.last_seen.clone();
                existing.price_usd = token.price_usd;
                existing.volume_24h_usd = token.volume_24h_usd;
            } else {
                token_uni.tokens.push(token.clone());
            }
            token_uni.chain = chain_lower.clone();
            token_uni.last_updated = chrono::Utc::now().to_rfc3339();
            if let Err(e) = store.save_tokens(&chain_lower, &token_uni) {
                warn!(error = %e, "Failed to save token universe");
            }

            info!(
                chain = %chain,
                token = %token.symbol,
                address = %token.address,
                volume = token.volume_24h_usd,
                total_tokens = token_uni.tokens.len(),
                "Token discovered (standard API)"
            );
            continue;
        }

        match enrich::enrich_candidate(&client, &candidate, &enrich_cfg, &store).await {
            Ok(Some((token, pools))) => {
                let kept_pools = filter_cfg.filter_pools(pools);

                if kept_pools.is_empty() {
                    info!(
                        token = %token.address,
                        symbol = %token.symbol,
                        "No pools pass filter — skipping"
                    );
                    continue;
                }

                if !filter_cfg.should_keep_token(&token) {
                    info!(
                        token = %token.address,
                        symbol = %token.symbol,
                        "Token fails filter — skipping"
                    );
                    continue;
                }

                let mut pool_uni = store.load_pools(&chain_lower).unwrap_or_default();
                for pool in &kept_pools {
                    if !pool_uni.pools.iter().any(|p| p.address == pool.address) {
                        pool_uni.pools.push(pool.clone());
                    }
                }
                pool_uni.chain = chain_lower.clone();
                pool_uni.last_updated = chrono::Utc::now().to_rfc3339();
                if let Err(e) = store.save_pools(&chain_lower, &pool_uni) {
                    warn!(error = %e, "Failed to save pool universe");
                }

                let mut token_uni = store.load_tokens(&chain_lower).unwrap_or_default();
                let existing = token_uni.tokens.iter_mut()
                    .find(|t| t.address.to_lowercase() == token.address.to_lowercase());
                if let Some(existing) = existing {
                    existing.last_seen = token.last_seen.clone();
                    existing.price_usd = token.price_usd;
                    existing.liquidity_usd = token.liquidity_usd;
                    existing.volume_24h_usd = token.volume_24h_usd;
                } else {
                    token_uni.tokens.push(token.clone());
                }
                token_uni.chain = chain_lower.clone();
                token_uni.last_updated = chrono::Utc::now().to_rfc3339();
                if let Err(e) = store.save_tokens(&chain_lower, &token_uni) {
                    warn!(error = %e, "Failed to save token universe");
                }

                info!(
                    chain = %chain,
                    token = %token.symbol,
                    pools = kept_pools.len(),
                    total_pools = pool_uni.pools.len(),
                    total_tokens = token_uni.tokens.len(),
                    "Discovery updated"
                );
            }
            Ok(None) => {}
            Err(e) => {
                warn!(
                    token = %candidate.address,
                    error = %e,
                    "Enrichment failed"
                );
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    info!("arb-discover starting");

    let api_key = std::env::var("CMC_API_KEY")
        .map_err(|_| anyhow::anyhow!("Missing CMC_API_KEY env var"))?;

    let client = Arc::new(CmcClient::new(&api_key, 250, 75_000.0)?);

    info!("Verifying CMC API key...");
    match client.key_info().await {
        Ok(info_val) => {
            let plan = info_val.get("data")
                .and_then(|d| d.get("plan"))
                .and_then(|p| p.get("credit_limit_monthly"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            info!(monthly_credits = plan, "CMC API key validated");
        }
        Err(e) => {
            error!(error = %e, "CMC API key validation failed");
            return Err(e);
        }
    }

    // Auto-detect by trying a real DEX data endpoint
    let use_standard_api = match client.dex_gainer_loser_list("1").await {
        Ok(_) => {
            info!("DEX API endpoints accessible — using DEX feeds");
            false
        }
        Err(e) => {
            let msg = format!("{e}");
            if msg.contains("1006") || msg.contains("doesn't support") {
                info!("DEX API not on this plan — falling back to standard API feeds");
                true
            } else {
                info!("DEX API probe returned error (may be transient) — using standard API feeds");
                true
            }
        }
    };

    let store = Arc::new(DiscoveryStore::new(Path::new("discovery")));

    let args: Vec<String> = std::env::args().collect();
    let chains: Vec<&str> = if args.len() > 1 {
        args[1..].iter().map(|s| s.as_str()).collect()
    } else {
        vec!["BSC", "Base"]
    };

    let mut handles = Vec::new();
    for chain in chains {
        let c = client.clone();
        let s = store.clone();
        let chain_owned = chain.to_string();
        let std_api = use_standard_api;
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_chain(&chain_owned, c, s, std_api).await {
                error!(chain = %chain_owned, error = %e, "Chain discovery failed");
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    Ok(())
}
