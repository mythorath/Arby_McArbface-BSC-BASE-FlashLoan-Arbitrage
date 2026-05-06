use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::cmc::{CmcClient, ListingEntry, MemeTokenEntry, TokenLeaderboardEntry};
use crate::store::DiscoveredToken;

/// A candidate token address discovered by one of the feeds.
#[derive(Debug, Clone)]
pub struct FeedCandidate {
    pub address: String,
    pub source: String,
    pub token: Option<DiscoveredToken>,
}

/// Platform config for a chain's CMC platform ID and name slug.
#[derive(Debug, Clone)]
pub struct PlatformConfig {
    pub platform_id: i32,
    pub platform_ids_str: String,
    pub platform_name: String,
    pub chain_name: String,
}

impl PlatformConfig {
    pub fn bsc() -> Self {
        Self {
            platform_id: 1,
            platform_ids_str: "1".to_string(),
            platform_name: "bsc".to_string(),
            chain_name: "BSC".to_string(),
        }
    }

    pub fn base() -> Self {
        Self {
            platform_id: 131,
            platform_ids_str: "131".to_string(),
            platform_name: "base".to_string(),
            chain_name: "Base".to_string(),
        }
    }
}

fn leaderboard_to_token(entry: &TokenLeaderboardEntry, source: &str) -> Option<DiscoveredToken> {
    let addr = entry.addr.as_ref()?;
    let now = chrono::Utc::now().to_rfc3339();

    Some(DiscoveredToken {
        address: addr.clone(),
        symbol: entry.sym.clone().unwrap_or_default(),
        name: entry.n.clone().unwrap_or_default(),
        decimals: entry.dec.unwrap_or(18).max(0) as u32,
        price_usd: entry.p.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        market_cap_usd: entry.mcap.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        liquidity_usd: entry.liq_usd.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        volume_24h_usd: entry.v24h.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        security_level: entry.rl.clone().unwrap_or_default(),
        honeypot_status: String::new(),
        buy_tax_bps: 0,
        sell_tax_bps: 0,
        is_flagged: false,
        holder_count: entry.hcnt.unwrap_or(0).max(0) as u64,
        top_holder_rate: entry.thr.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        source: source.to_string(),
        first_seen: now.clone(),
        last_seen: now,
        platform_id: entry.pcid.unwrap_or(0).max(0) as u32,
    })
}

fn meme_to_token(entry: &MemeTokenEntry, source: &str) -> Option<DiscoveredToken> {
    let addr = entry.addr.as_ref()?;
    let now = chrono::Utc::now().to_rfc3339();

    let parse_f64 = |v: &Option<serde_json::Value>| -> f64 {
        v.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
            serde_json::Value::String(s) => s.parse().unwrap_or(0.0),
            _ => 0.0,
        }).unwrap_or(0.0)
    };

    Some(DiscoveredToken {
        address: addr.clone(),
        symbol: entry.sym.clone().unwrap_or_default(),
        name: entry.n.clone().unwrap_or_default(),
        decimals: entry.dec.unwrap_or(18).max(0) as u32,
        price_usd: parse_f64(&entry.p),
        market_cap_usd: parse_f64(&entry.mcap),
        liquidity_usd: parse_f64(&entry.liq),
        volume_24h_usd: parse_f64(&entry.vu),
        security_level: String::new(),
        honeypot_status: String::new(),
        buy_tax_bps: 0,
        sell_tax_bps: 0,
        is_flagged: false,
        holder_count: entry.h.unwrap_or(0).max(0) as u64,
        top_holder_rate: entry.htp.unwrap_or(0.0),
        source: source.to_string(),
        first_seen: now.clone(),
        last_seen: now,
        platform_id: entry.plt.unwrap_or(0).max(0) as u32,
    })
}

fn listing_to_token(entry: &ListingEntry, chain_filter: &str, source: &str) -> Option<DiscoveredToken> {
    let plat = entry.platform.as_ref()?;
    let chain_name = plat.name.as_deref().unwrap_or("");
    let matches_chain = match chain_filter {
        "BSC" | "bsc" => chain_name.contains("BNB"),
        "Base" | "base" => chain_name.contains("Base"),
        _ => false,
    };
    if !matches_chain { return None; }
    let addr = plat.token_address.as_ref()?;
    if addr.is_empty() { return None; }

    let now = chrono::Utc::now().to_rfc3339();
    let quote = entry.quote.as_ref()
        .and_then(|q| q.get("USD"));

    Some(DiscoveredToken {
        address: addr.clone(),
        symbol: entry.symbol.clone().unwrap_or_default(),
        name: entry.name.clone().unwrap_or_default(),
        decimals: 18,
        price_usd: quote.and_then(|q| q.price).unwrap_or(0.0),
        market_cap_usd: quote.and_then(|q| q.market_cap).unwrap_or(0.0),
        liquidity_usd: 0.0,
        volume_24h_usd: quote.and_then(|q| q.volume_24h).unwrap_or(0.0),
        security_level: String::new(),
        honeypot_status: String::new(),
        buy_tax_bps: 0,
        sell_tax_bps: 0,
        is_flagged: false,
        holder_count: 0,
        top_holder_rate: 0.0,
        source: source.to_string(),
        first_seen: now.clone(),
        last_seen: now,
        platform_id: 0,
    })
}

pub struct FeedConfig {
    pub new_list_interval: Duration,
    pub meme_list_interval: Duration,
    pub gainer_loser_interval: Duration,
    pub min_liquidity_usd: f64,
    pub max_age_minutes: u32,
    pub use_standard_api: bool,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            new_list_interval: Duration::from_secs(300),
            meme_list_interval: Duration::from_secs(600),
            gainer_loser_interval: Duration::from_secs(600),
            min_liquidity_usd: 25_000.0,
            max_age_minutes: 7 * 24 * 60,
            use_standard_api: false,
        }
    }
}

/// Runs the periodic CMC feed polling loop. Sends new candidate addresses
/// through the channel for enrichment.
pub async fn run_feeds(
    client: Arc<CmcClient>,
    platform: PlatformConfig,
    feed_cfg: FeedConfig,
    tx: mpsc::Sender<FeedCandidate>,
) -> Result<()> {
    let mut seen: HashSet<String> = HashSet::new();
    const MAX_SEEN: usize = 100_000;
    let mut new_tick = tokio::time::interval(feed_cfg.new_list_interval);
    let mut meme_tick = tokio::time::interval(feed_cfg.meme_list_interval);
    let mut gainer_tick = tokio::time::interval(feed_cfg.gainer_loser_interval);

    info!(
        chain = %platform.chain_name,
        new_interval_s = feed_cfg.new_list_interval.as_secs(),
        meme_interval_s = feed_cfg.meme_list_interval.as_secs(),
        "Feed loop starting"
    );

    let use_std = feed_cfg.use_standard_api;

    loop {
        if seen.len() > MAX_SEEN {
            seen.clear();
            info!(chain = %platform.chain_name, "Cleared seen set (exceeded {})", MAX_SEEN);
        }
        tokio::select! {
            _ = new_tick.tick() => {
                if use_std {
                    match client.listings_newest(100, 10_000.0).await {
                        Ok(entries) => {
                            let mut new_count = 0;
                            for entry in &entries {
                                if let Some(token) = listing_to_token(entry, &platform.chain_name, "cmc_new") {
                                    let key = token.address.to_lowercase();
                                    if seen.insert(key) {
                                        let addr = token.address.clone();
                                        let _ = tx.send(FeedCandidate {
                                            address: addr,
                                            source: "cmc_new".to_string(),
                                            token: Some(token),
                                        }).await;
                                        new_count += 1;
                                    }
                                }
                            }
                            if new_count > 0 {
                                info!(chain = %platform.chain_name, total = entries.len(),
                                    new = new_count, "New token feed (standard API)");
                            }
                        }
                        Err(e) => warn!(chain = %platform.chain_name, error = %e, "New token feed failed"),
                    }
                } else {
                    match client.dex_new_list(
                        &platform.platform_ids_str,
                        feed_cfg.min_liquidity_usd,
                        feed_cfg.max_age_minutes,
                    ).await {
                        Ok(entries) => {
                            let mut new_count = 0;
                            for entry in &entries {
                                if let Some(addr) = &entry.addr {
                                    let key = addr.to_lowercase();
                                    if seen.insert(key) {
                                        if let Some(token) = leaderboard_to_token(entry, "cmc_new") {
                                            let _ = tx.send(FeedCandidate {
                                                address: addr.clone(),
                                                source: "cmc_new".to_string(),
                                                token: Some(token),
                                            }).await;
                                            new_count += 1;
                                        }
                                    }
                                }
                            }
                            if new_count > 0 {
                                info!(chain = %platform.chain_name, total = entries.len(),
                                    new = new_count, "New token feed");
                            }
                        }
                        Err(e) => warn!(chain = %platform.chain_name, error = %e, "New token feed failed"),
                    }
                }
            }

            _ = meme_tick.tick() => {
                if use_std {
                    // Standard API doesn't have a meme-specific endpoint;
                    // skip this tick (gainers catch volatile memecoins)
                } else {
                    match client.dex_meme_list(platform.platform_id).await {
                        Ok(entries) => {
                            let mut new_count = 0;
                            for entry in &entries {
                                if let Some(addr) = &entry.addr {
                                    let key = addr.to_lowercase();
                                    if seen.insert(key) {
                                        if let Some(token) = meme_to_token(entry, "cmc_meme") {
                                            let _ = tx.send(FeedCandidate {
                                                address: addr.clone(),
                                                source: "cmc_meme".to_string(),
                                                token: Some(token),
                                            }).await;
                                            new_count += 1;
                                        }
                                    }
                                }
                            }
                            if new_count > 0 {
                                info!(chain = %platform.chain_name, total = entries.len(),
                                    new = new_count, "Meme token feed");
                            }
                        }
                        Err(e) => warn!(chain = %platform.chain_name, error = %e, "Meme feed failed"),
                    }
                }
            }

            _ = gainer_tick.tick() => {
                if use_std {
                    match client.listings_gainers(100, 10_000.0).await {
                        Ok(entries) => {
                            let mut new_count = 0;
                            for entry in &entries {
                                if let Some(token) = listing_to_token(entry, &platform.chain_name, "cmc_gainer") {
                                    let key = token.address.to_lowercase();
                                    if seen.insert(key) {
                                        let addr = token.address.clone();
                                        let _ = tx.send(FeedCandidate {
                                            address: addr,
                                            source: "cmc_gainer".to_string(),
                                            token: Some(token),
                                        }).await;
                                        new_count += 1;
                                    }
                                }
                            }
                            if new_count > 0 {
                                info!(chain = %platform.chain_name, total = entries.len(),
                                    new = new_count, "Gainer feed (standard API)");
                            }
                        }
                        Err(e) => warn!(chain = %platform.chain_name, error = %e, "Gainer feed failed"),
                    }
                } else {
                    match client.dex_gainer_loser_list(&platform.platform_ids_str).await {
                        Ok(entries) => {
                            let mut new_count = 0;
                            for entry in &entries {
                                if let Some(addr) = &entry.addr {
                                    let key = addr.to_lowercase();
                                    if seen.insert(key) {
                                        if let Some(token) = leaderboard_to_token(entry, "cmc_gainer") {
                                            let _ = tx.send(FeedCandidate {
                                                address: addr.clone(),
                                                source: "cmc_gainer".to_string(),
                                                token: Some(token),
                                            }).await;
                                            new_count += 1;
                                        }
                                    }
                                }
                            }
                            if new_count > 0 {
                                info!(chain = %platform.chain_name, total = entries.len(),
                                    new = new_count, "Gainer/loser feed");
                            }
                        }
                        Err(e) => warn!(chain = %platform.chain_name, error = %e, "Gainer feed failed"),
                    }
                }
            }
        }
    }
}
