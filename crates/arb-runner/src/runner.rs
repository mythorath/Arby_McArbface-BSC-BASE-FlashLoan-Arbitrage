use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::{Address, B256, U256};
use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use arb_discovery::store::DiscoveryStore;
use arb_mempool::MempoolWatcher;
use arb_paths::enumerate::{PathEnumerator, PoolInfo};
use arb_paths::PathTemplate;
use arb_rpc::Endpoint;
use arb_sim::evaluate::evaluate_all;
use arb_sim::gate::ProfitGate;
use arb_sim::optimize::{find_optimal_amount, path_max_flash};
use arb_state::pool_store::PoolStore;
use arb_state::refresher::{PoolConfig, StateRefresher};
use arb_submit::blink::BlinkSubmitter;
use arb_submit::blockrazor::BlockRazorSubmitter;
use arb_submit::direct::DirectSubmitter;
use arb_submit::jetbldr::JetBldrSubmitter;
use arb_submit::nodereal::NodeRealSubmitter;
use arb_submit::puissant::PuissantSubmitter;
use arb_submit::warp::WarpSubmitter;
use arb_submit::presign::PresignPool;
use arb_submit::{SubmitTier, Submitter};

use crate::config::AppConfig;
use crate::metrics;

const CIRCUIT_BREAKER_MAX_REVERTS: u32 = 3;
const CIRCUIT_BREAKER_SUPPRESS_BLOCKS: u64 = 30;
const CIRCUIT_BREAKER_DECAY_BLOCKS: u64 = 100;

struct PathCircuitBreaker {
    stats: HashMap<u32, PathStats>,
}

struct PathStats {
    consecutive_reverts: u32,
    last_revert_block: u64,
    suppressed_until_block: u64,
    total_submits: u64,
    total_reverts: u64,
    total_successes: u64,
}

impl PathCircuitBreaker {
    fn new() -> Self {
        Self { stats: HashMap::new() }
    }

    fn is_suppressed(&self, path_id: u32, current_block: u64) -> bool {
        if let Some(s) = self.stats.get(&path_id) {
            current_block < s.suppressed_until_block
        } else {
            false
        }
    }

    fn record_submit(&mut self, path_id: u32) {
        let s = self.stats.entry(path_id).or_insert(PathStats {
            consecutive_reverts: 0, last_revert_block: 0, suppressed_until_block: 0,
            total_submits: 0, total_reverts: 0, total_successes: 0,
        });
        s.total_submits += 1;
    }

    fn record_revert(&mut self, path_id: u32, block: u64) {
        let s = self.stats.entry(path_id).or_insert(PathStats {
            consecutive_reverts: 0, last_revert_block: 0, suppressed_until_block: 0,
            total_submits: 0, total_reverts: 0, total_successes: 0,
        });
        s.total_reverts += 1;
        if block > s.last_revert_block + CIRCUIT_BREAKER_DECAY_BLOCKS {
            s.consecutive_reverts = 0;
        }
        s.consecutive_reverts += 1;
        s.last_revert_block = block;
        if s.consecutive_reverts >= CIRCUIT_BREAKER_MAX_REVERTS {
            s.suppressed_until_block = block + CIRCUIT_BREAKER_SUPPRESS_BLOCKS;
            metrics::PATH_SUPPRESSED.inc();
            warn!(path_id, until_block = s.suppressed_until_block, "Path circuit-breaker tripped");
        }
    }

    fn record_success(&mut self, path_id: u32) {
        let s = self.stats.entry(path_id).or_insert(PathStats {
            consecutive_reverts: 0, last_revert_block: 0, suppressed_until_block: 0,
            total_submits: 0, total_reverts: 0, total_successes: 0,
        });
        s.consecutive_reverts = 0;
        s.suppressed_until_block = 0;
        s.total_successes += 1;
    }

    fn suppressed_count(&self, current_block: u64) -> usize {
        self.stats.values().filter(|s| current_block < s.suppressed_until_block).count()
    }

    /// Top-10 paths by total submissions (most active).
    fn top_active(&self) -> Vec<(u32, u64, u64, u64)> {
        let mut entries: Vec<_> = self.stats.iter()
            .filter(|(_, s)| s.total_submits > 0)
            .map(|(&pid, s)| (pid, s.total_submits, s.total_reverts, s.total_successes))
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries.truncate(10);
        entries
    }

    /// Bottom-10 paths by revert rate (highest revert %).
    fn worst_revert_rate(&self) -> Vec<(u32, u64, u64, f64)> {
        let mut entries: Vec<_> = self.stats.iter()
            .filter(|(_, s)| s.total_submits >= 3)
            .map(|(&pid, s)| {
                let rate = s.total_reverts as f64 / s.total_submits as f64;
                (pid, s.total_submits, s.total_reverts, rate)
            })
            .collect();
        entries.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
        entries.truncate(10);
        entries
    }
}

struct TokenCircuitBreaker {
    stats: HashMap<Address, TokenBreakerStats>,
    revert_threshold: u32,
    suppression_blocks: u64,
    blacklist: HashSet<Address>,
    popular_intermediaries: HashSet<Address>,
}

struct TokenBreakerStats {
    consecutive_reverts: u32,
    last_revert_block: u64,
    suppressed_until_block: u64,
}

impl TokenCircuitBreaker {
    fn new(revert_threshold: u32, suppression_blocks: u64) -> Self {
        Self {
            stats: HashMap::new(),
            revert_threshold,
            suppression_blocks,
            blacklist: HashSet::new(),
            popular_intermediaries: HashSet::new(),
        }
    }

    fn set_popular_intermediaries(&mut self, tokens: impl IntoIterator<Item = Address>) {
        self.popular_intermediaries = tokens.into_iter().collect();
    }

    fn load_blacklist(&mut self, path: &std::path::Path) {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(bl) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(addrs) = bl.get("addresses").and_then(|a| a.as_array()) {
                    for entry in addrs {
                        if let Some(addr_str) = entry.get("address").and_then(|a| a.as_str()) {
                            if let Ok(addr) = addr_str.parse::<Address>() {
                                self.blacklist.insert(addr);
                            }
                        }
                    }
                }
                info!(count = self.blacklist.len(), "Loaded token blacklist");
            }
        }
    }

    fn is_token_suppressed(&self, token: Address, current_block: u64) -> bool {
        if self.blacklist.contains(&token) {
            return true;
        }
        if let Some(s) = self.stats.get(&token) {
            current_block < s.suppressed_until_block
        } else {
            false
        }
    }

    fn is_path_token_suppressed(&self, path: &PathTemplate, current_block: u64) -> bool {
        path.hops.iter().any(|hop| {
            self.is_token_suppressed(hop.token_in, current_block)
                || self.is_token_suppressed(hop.token_out, current_block)
        })
    }

    fn record_revert_for_path(&mut self, path: &PathTemplate, block: u64) {
        for hop in &path.hops {
            for &token in &[hop.token_in, hop.token_out] {
                if token == path.flash_token || self.popular_intermediaries.contains(&token) {
                    continue;
                }
                let s = self.stats.entry(token).or_insert(TokenBreakerStats {
                    consecutive_reverts: 0,
                    last_revert_block: 0,
                    suppressed_until_block: 0,
                });
                if block > s.last_revert_block + 200 {
                    s.consecutive_reverts = 0;
                }
                s.consecutive_reverts += 1;
                s.last_revert_block = block;
                if s.consecutive_reverts >= self.revert_threshold {
                    s.suppressed_until_block = block + self.suppression_blocks;
                    warn!(token = %token, until_block = s.suppressed_until_block,
                        "Token circuit-breaker tripped");
                }
            }
        }
    }

    fn record_success_for_path(&mut self, path: &PathTemplate) {
        for hop in &path.hops {
            for &token in &[hop.token_in, hop.token_out] {
                if let Some(s) = self.stats.get_mut(&token) {
                    s.consecutive_reverts = 0;
                    s.suppressed_until_block = 0;
                }
            }
        }
    }

    fn suppressed_token_count(&self, current_block: u64) -> usize {
        self.blacklist.len()
            + self.stats.values().filter(|s| current_block < s.suppressed_until_block).count()
    }
}

fn is_nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|v| !v.is_empty())
}

enum TxOutcome { Success, Revert, Dropped }

/// Track a submitted tx hash — poll for receipt, update metrics.
/// Returns the outcome so the caller can feed the circuit breaker.
async fn track_tx(endpoint: Arc<Endpoint>, tx_hash: B256, deadline_blocks: u64) -> TxOutcome {
    let start_block = endpoint.block_number().await.unwrap_or(0);
    let max_polls = (deadline_blocks * 4).max(8);
    let poll_interval_ms = 750;

    for _ in 0..max_polls {
        match endpoint.get_receipt(tx_hash).await {
            Ok(Some(receipt)) => {
                let success = receipt.status();
                let label = if success { "success" } else { "revert" };
                metrics::SUBMIT_LANDED.with_label_values(&[label]).inc();
                let gas_cost = receipt.gas_used as f64 * receipt.effective_gas_price as f64;
                metrics::GAS_SPENT_WEI.inc_by(gas_cost);
                info!(
                    tx = %tx_hash,
                    status = label,
                    gas_used = receipt.gas_used,
                    "Tx landed on-chain"
                );
                return if success { TxOutcome::Success } else { TxOutcome::Revert };
            }
            Ok(None) => {}
            Err(e) => {
                debug!(tx = %tx_hash, error = %e, "Receipt fetch error");
            }
        }
        if endpoint.block_number().await.unwrap_or(0) > start_block + deadline_blocks {
            metrics::SUBMIT_LANDED.with_label_values(&["dropped"]).inc();
            debug!(tx = %tx_hash, "Tx dropped (not mined within deadline)");
            return TxOutcome::Dropped;
        }
        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
    }
    metrics::SUBMIT_LANDED.with_label_values(&["dropped"]).inc();
    TxOutcome::Dropped
}

/// Write status JSON snapshot for monitoring.
fn write_status_json(
    chain_name: &str,
    started: &chrono::DateTime<chrono::Utc>,
    block: u64,
    wallet_balance: &str,
    cb: &PathCircuitBreaker,
) {
    let now = chrono::Utc::now();
    let uptime = (now - *started).num_seconds();

    let landed_ok = metrics::SUBMIT_LANDED.with_label_values(&["success"]).get() as u64;
    let landed_revert = metrics::SUBMIT_LANDED.with_label_values(&["revert"]).get() as u64;
    let dropped = metrics::SUBMIT_LANDED.with_label_values(&["dropped"]).get() as u64;

    let top_active: Vec<_> = cb.top_active().iter().map(|(pid, sub, rev, suc)| {
        serde_json::json!({"path_id": pid, "submits": sub, "reverts": rev, "successes": suc})
    }).collect();

    let worst_revert: Vec<_> = cb.worst_revert_rate().iter().map(|(pid, sub, rev, rate)| {
        serde_json::json!({"path_id": pid, "submits": sub, "reverts": rev, "revert_pct": format!("{:.0}", rate * 100.0)})
    }).collect();

    let status = serde_json::json!({
        "chain": chain_name,
        "started": started.to_rfc3339(),
        "last_update": now.to_rfc3339(),
        "block": block,
        "uptime_seconds": uptime,
        "metrics": {
            "scans_total": metrics::PATHS_EVALUATED.get() as u64,
            "candidates_total": metrics::PROFITABLE_FOUND.get() as u64,
            "submitted_total": metrics::SUBMIT_ATTEMPTS.get() as u64,
            "landed_success": landed_ok,
            "landed_revert": landed_revert,
            "dropped": dropped,
            "paths_suppressed": cb.suppressed_count(block),
            "backrun_candidates": metrics::BACKRUN_CANDIDATES.get() as u64,
            "backrun_submitted": metrics::BACKRUN_SUBMITTED.get() as u64,
            "warp_spend_usd": format!("{:.2}", metrics::WARP_SPEND_USD.get()),
        },
        "wallet_balance_native": wallet_balance,
        "top_active_paths": top_active,
        "worst_revert_paths": worst_revert,
    });

    let dir = std::path::Path::new("status");
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("{}.json", chain_name.to_lowercase()));
    let _ = std::fs::write(path, serde_json::to_string_pretty(&status).unwrap_or_default());
}

pub async fn run(cfg: AppConfig, smoke_test: bool) -> Result<()> {
    let started = chrono::Utc::now();
    let chain_name = cfg.chain.name.clone();

    let pk_env = &cfg.wallet.private_key_env;
    let private_key = std::env::var(pk_env)
        .map_err(|_| anyhow::anyhow!("Missing env var {pk_env}"))?;
    let signer: PrivateKeySigner = private_key.parse()?;
    info!(address = %signer.address(), "Wallet loaded");

    let trader_url = cfg.chain.trader_rpc.as_deref();
    let endpoint = Arc::new(
        Endpoint::new(&cfg.chain.rpc_https, &cfg.chain.rpc_wss, trader_url, cfg.chain.chain_id).await?,
    );

    let tokens: HashMap<String, Address> = cfg
        .tokens
        .iter()
        .map(|(name, addr_str)| {
            let addr: Address = addr_str.parse().expect("Invalid token address");
            (name.clone(), addr)
        })
        .collect();

    let token_usd_prices: HashMap<Address, f64> = cfg
        .token_usd_prices
        .iter()
        .filter_map(|(name, &price)| tokens.get(name).map(|&addr| (addr, price)))
        .collect();

    let mut pool_configs: Vec<PoolConfig> = cfg
        .pools
        .iter()
        .map(|p| PoolConfig {
            address: p.address.parse().expect("Invalid pool address"),
            protocol: p.parse_protocol(),
            fee_bps: p.fee_bps,
            token0: tokens.get(&p.token0).copied(),
            token1: tokens.get(&p.token1).copied(),
        })
        .collect();

    let mut pool_infos: Vec<PoolInfo> = cfg
        .pools
        .iter()
        .map(|p| PoolInfo {
            address: p.address.parse().expect("Invalid pool address"),
            protocol: p.parse_protocol(),
            token0: tokens[&p.token0],
            token1: tokens[&p.token1],
        })
        .collect();

    let mut token_usd_prices = token_usd_prices;
    let mut token_decimals: HashMap<Address, u32> = HashMap::new();

    // Populate decimals for known tokens from config
    for (name, &addr) in &tokens {
        let dec = match name.as_str() {
            "USDT" | "USDC" | "USDbC" | "BUSD" => 6,
            _ => 18,
        };
        token_decimals.insert(addr, dec);
    }

    // Merge discovered pools/tokens from arb-discovery JSON files (async I/O)
    let discovery_store = DiscoveryStore::new(std::path::Path::new("discovery"));
    let chain_lower = chain_name.to_lowercase();
    let toml_pool_addrs: HashSet<Address> = pool_configs.iter().map(|p| p.address).collect();

    if let Ok(pool_universe) = discovery_store.load_pools_async(&chain_lower).await {
        let mut merged = 0u32;
        for dp in &pool_universe.pools {
            let addr: Address = match dp.address.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if toml_pool_addrs.contains(&addr) {
                continue;
            }
            let t0: Address = match dp.token0.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let t1: Address = match dp.token1.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let protocol = crate::config::PoolEntry {
                name: format!("DISC_{}", dp.exchange_name.replace(' ', "_")),
                address: dp.address.clone(),
                protocol: dp.protocol.clone(),
                token0: dp.token0.clone(),
                token1: dp.token1.clone(),
                fee_bps: dp.fee_bps,
            }
            .parse_protocol();

            pool_configs.push(PoolConfig {
                address: addr,
                protocol,
                fee_bps: dp.fee_bps,
                token0: Some(t0),
                token1: Some(t1),
            });
            pool_infos.push(PoolInfo {
                address: addr,
                protocol,
                token0: t0,
                token1: t1,
            });
            merged += 1;
        }
        if merged > 0 {
            info!(merged, total = pool_infos.len(), "Merged discovered pools");
        }
    }

    if let Ok(token_universe) = discovery_store.load_tokens_async(&chain_lower).await {
        let mut price_merged = 0u32;
        for dt in &token_universe.tokens {
            let addr: Address = match dt.address.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            // Only set price if not already known from TOML
            if !token_usd_prices.contains_key(&addr) && dt.price_usd > 0.0 {
                token_usd_prices.insert(addr, dt.price_usd);
                price_merged += 1;
            }
            token_decimals.insert(addr, dt.decimals);
        }
        if price_merged > 0 {
            info!(price_merged, "Merged discovered token prices");
        }
    }

    let store = Arc::new(PoolStore::new());
    let state_reader: Address = cfg.chain.state_reader.parse()?;
    let refresher = StateRefresher::new(endpoint.clone(), state_reader, pool_configs, cfg.chain.chain_id);

    let (count, elapsed) = refresher.refresh(&store).await?;
    info!(pools = count, elapsed_ms = elapsed.as_millis(), "Initial state refresh complete");
    metrics::POOL_COUNT.set(count as f64);

    let pricing_refresh_blocks: u64 = 100;
    let derived = crate::pricing::derive_prices(&store, &mut token_usd_prices, &token_decimals);
    info!(derived, total_priced = token_usd_prices.len(), "Initial price derivation complete");

    let flash_tokens: Vec<Address> = cfg.scanner.flash_tokens.iter().map(|name| tokens[name]).collect();
    let flash_amounts: HashMap<Address, U256> = cfg.scanner.flash_amounts.iter()
        .map(|(name, &amount)| (tokens[name], U256::from(amount))).collect();
    let flash_bounds: HashMap<Address, (U256, U256)> = cfg.scanner.flash_bounds.iter()
        .filter_map(|(name, bounds)| tokens.get(name).map(|&addr| (addr, (U256::from(bounds.min), U256::from(bounds.max)))))
        .collect();

    let enumerator = PathEnumerator::new(pool_infos, flash_tokens, flash_amounts);
    let paths = enumerator.enumerate();
    info!(total_paths = paths.len(), "Path enumeration complete");

    let presign_pool = PresignPool::new(&paths, cfg.chain.chain_id);

    // Build index: (token_in, token_out) -> vec of path indices that touch that pair.
    // Used for fast backrun lookups when a pending swap is detected.
    let mut token_pair_to_paths: HashMap<(Address, Address), Vec<usize>> = HashMap::new();
    for (idx, path) in paths.iter().enumerate() {
        for hop in &path.hops {
            token_pair_to_paths.entry((hop.token_in, hop.token_out)).or_default().push(idx);
        }
    }

    // ===== Submitters =====
    let mut submitters: Vec<Box<dyn Submitter>> = Vec::new();
    let chain_label: &'static str = if cfg.chain.chain_id == 8453 { "Base" } else { "BSC" };

    if cfg.chain.chain_id == 56 {
        if let Some(url) = is_nonempty(&cfg.submission.puissant_url) {
            submitters.push(Box::new(PuissantSubmitter::new(url)));
            info!("48Club Puissant v2 configured");
        }
        if let Some(url) = is_nonempty(&cfg.submission.blockrazor_url) {
            submitters.push(Box::new(BlockRazorSubmitter::new(url)));
            info!("BlockRazor configured");
        }
        if let Some(url) = is_nonempty(&cfg.submission.jetbldr_url) {
            submitters.push(Box::new(JetBldrSubmitter::new(url)));
            info!("JetBldr configured");
        }
        if let Some(url) = is_nonempty(&cfg.submission.nodereal_url) {
            submitters.push(Box::new(NodeRealSubmitter::new(url)));
            info!("NodeReal configured");
        }
    }
    if let Some(url) = is_nonempty(&cfg.submission.blink_url) {
        submitters.push(Box::new(BlinkSubmitter::new(url, chain_label)));
        info!(chain = chain_label, "Blink configured");
    }
    if endpoint.has_trader_endpoint() {
        submitters.push(Box::new(WarpSubmitter::new(endpoint.clone())));
        info!("Warp/Trader configured (HighEvOnly)");
    }
    if cfg.submission.direct_fallback {
        submitters.push(Box::new(DirectSubmitter::new(endpoint.clone())));
        info!("Direct RPC fallback configured");
    }

    let always_on = submitters.iter().filter(|s| s.tier() == SubmitTier::AlwaysOn).count();
    let high_ev = submitters.iter().filter(|s| s.tier() == SubmitTier::HighEvOnly).count();
    info!(always_on, high_ev, total = submitters.len(), "Submission layer initialized");

    let mut profit_gate = if smoke_test {
        ProfitGate::new(0, 0.0, 0, 0, token_usd_prices.clone(), token_decimals.clone())
    } else {
        ProfitGate::new(
            cfg.scanner.min_profit_bps, cfg.gate.min_profit_usd,
            cfg.gate.safety_margin_bps, cfg.gate.stable_pool_extra_margin_bps,
            token_usd_prices.clone(), token_decimals.clone(),
        )
    };
    info!(min_bps = cfg.scanner.min_profit_bps, min_usd = cfg.gate.min_profit_usd, "Profit gate initialized");

    let warp_threshold_usd = cfg.submission.warp_threshold_usd;
    let warp_budget_usd = cfg.submission.warp_budget_usd;
    let mut warp_spent_this_session: f64 = 0.0;
    info!(
        threshold_usd = warp_threshold_usd,
        budget_usd = warp_budget_usd,
        "Warp spending limits loaded"
    );
    let min_initial_bps = if smoke_test { 0 } else { cfg.scanner.min_initial_bps };
    let optimization_iterations = cfg.scanner.optimization_iterations;
    let wallet_addr = signer.address();
    let dry_run_override = if smoke_test { false } else { cfg.scanner.dry_run };

    let (mempool_tx, mut mempool_rx) = mpsc::channel(1000);
    let wss_url = cfg.chain.rpc_wss.clone();
    let mempool_chain_id = cfg.chain.chain_id;
    tokio::spawn(async move {
        let watcher = MempoolWatcher::new(&wss_url, mempool_chain_id);
        if let Err(e) = watcher.start(mempool_tx).await {
            error!(error = %e, "Mempool watcher failed");
        }
    });

    info!("Subscribing to new block headers");
    let ws = WsConnect::new(&cfg.chain.rpc_wss);
    let ws_provider = ProviderBuilder::new().connect_ws(ws).await?;
    let sub = ws_provider.subscribe_blocks().await?;
    let mut block_stream = sub.into_stream();

    let arb_contract: Address = cfg.chain.arb_contract.parse()?;
    let dry_run = dry_run_override;

    info!(chain = %cfg.chain.name, contract = %arb_contract, pools = store.pool_count(),
        paths = paths.len(), dry_run, "Scanner loop starting");

    let mut circuit_breaker = PathCircuitBreaker::new();
    let mut token_breaker = TokenCircuitBreaker::new(5, 200);
    token_breaker.set_popular_intermediaries(tokens.values().copied());
    let blacklist_path = std::path::Path::new("discovery")
        .join(format!("blacklist.{}.json", chain_name.to_lowercase()));
    token_breaker.load_blacklist(&blacklist_path);
    let (cb_tx, mut cb_rx) = mpsc::channel::<(u32, TxOutcome, u64)>(256);

    // Per-minute summary tracking
    let mut last_summary = Instant::now();
    let mut last_scans: f64 = 0.0;
    let mut last_submitted: f64 = 0.0;
    let mut last_status_write = Instant::now();
    let mut last_pricing_block: u64 = 0;

    // Background balance task (Phase 7f: move RPC out of hot scan loop)
    let balance_addr = wallet_addr;
    let balance_ep = endpoint.clone();
    let (balance_tx, balance_rx) = tokio::sync::watch::channel("unknown".to_string());
    tokio::spawn(async move {
        loop {
            match balance_ep.get_balance(balance_addr).await {
                Ok(b) => { let _ = balance_tx.send(format!("{b}")); }
                Err(_) => { let _ = balance_tx.send("unknown".to_string()); }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    while let Some(block) = block_stream.next().await {
        let block_number = block.inner.number;
        let scan_start = Instant::now();
        metrics::CURRENT_BLOCK.set(block_number as f64);

        // Drain async tx outcome feedback into circuit breaker
        while let Ok((path_id, outcome, blk)) = cb_rx.try_recv() {
            match outcome {
                TxOutcome::Success => circuit_breaker.record_success(path_id),
                TxOutcome::Revert => circuit_breaker.record_revert(path_id, blk),
                TxOutcome::Dropped => {}
            }
        }

        match refresher.refresh(&store).await {
            Ok((count, elapsed)) => {
                metrics::STATE_REFRESH_LATENCY.observe(elapsed.as_secs_f64());
                debug!(block = block_number, pools = count, refresh_ms = elapsed.as_millis(), "State refreshed");
            }
            Err(e) => {
                warn!(block = block_number, error = %e, "State refresh failed");
            }
        }

        if block_number >= last_pricing_block + pricing_refresh_blocks {
            let derived = crate::pricing::derive_prices(&store, &mut token_usd_prices, &token_decimals);
            if derived > 0 {
                profit_gate.token_usd_prices = token_usd_prices.clone();
                debug!(derived, block = block_number, "Periodic price derivation");
            }
            last_pricing_block = block_number;
        }

        // === Two-pass evaluate-then-optimize ===
        let initial_results = evaluate_all(&paths, &store);
        let pass1_count = initial_results.len();
        metrics::PATHS_EVALUATED.inc_by(paths.len() as f64);

        let candidates: Vec<_> = initial_results.into_iter()
            .filter(|r| r.profit_bps >= min_initial_bps)
            .filter(|r| !circuit_breaker.is_suppressed(r.path_id, block_number))
            .filter(|r| !token_breaker.is_path_token_suppressed(&paths[r.path_id as usize], block_number))
            .collect();

        if !candidates.is_empty() {
            metrics::PROFITABLE_FOUND.inc_by(candidates.len() as f64);

            let mut best_result = None;
            let mut best_path_idx = 0usize;
            let mut optimized_count = 0u32;

            for candidate in &candidates {
                let path = &paths[candidate.path_id as usize];
                let (min_amt, token_max) = flash_bounds.get(&path.flash_token).copied()
                    .unwrap_or((path.flash_amount, path.flash_amount * U256::from(10u32)));
                let liquidity_max = path_max_flash(path, &store, 0.05, token_max);
                let max_amt = token_max.min(liquidity_max);

                // Try ternary optimization; fall back to the default amount if optimization fails
                let (final_amount, final_profit) =
                    if let Some((opt_amount, opt_profit)) =
                        find_optimal_amount(path, &store, min_amt, max_amt, optimization_iterations)
                    {
                        (opt_amount, opt_profit)
                    } else if candidate.gross_profit > U256::ZERO {
                        // Optimization found nothing, but cheap-pass DID find profit at default amount
                        (candidate.flash_amount, candidate.gross_profit)
                    } else {
                        continue;
                    };

                optimized_count += 1;
                let profit_bps: u32 = if !final_amount.is_zero() {
                    ((final_profit * U256::from(10000u32)) / final_amount).try_into().unwrap_or(u32::MAX)
                } else { 0 };

                let opt_result = arb_sim::SimResult {
                    path_id: candidate.path_id, flash_token: path.flash_token,
                    flash_amount: final_amount, final_amount: final_amount + final_profit,
                    gross_profit: final_profit, profit_bps,
                };

                let decision = profit_gate.should_submit(&opt_result, path);
                if decision.accept {
                    if best_result.as_ref().map_or(true, |(_, d): &(arb_sim::SimResult, f64)| decision.effective_profit_usd > *d) {
                        best_path_idx = candidate.path_id as usize;
                        best_result = Some((opt_result, decision.effective_profit_usd));
                    }
                }
            }

            if let Some((best, effective_usd)) = best_result {
                info!(block = block_number, path_id = best.path_id, profit_bps = best.profit_bps,
                    gross_profit = %best.gross_profit, flash_amount = %best.flash_amount,
                    effective_usd = format!("{:.4}", effective_usd), pass1 = pass1_count,
                    candidates = candidates.len(), optimized = optimized_count, "Optimized path");

                if !dry_run {
                    let optimized_path = &paths[best_path_idx];

                    let target_block = block_number + 3;
                    match presign_pool.build_fast(
                        best.path_id, best.flash_amount, &endpoint, arb_contract, &signer, target_block,
                    ).await {
                        Ok(bundle) => {
                            endpoint.bump_nonce();
                            circuit_breaker.record_submit(best.path_id);
                            let budget_ok = warp_spent_this_session < warp_budget_usd;
                            let use_high_ev = budget_ok && effective_usd >= warp_threshold_usd;
                            if !budget_ok {
                                error!(
                                    spent = format!("{:.2}", warp_spent_this_session),
                                    budget = warp_budget_usd,
                                    "WARP BUDGET EXCEEDED — shutting down to prevent further charges"
                                );
                                return Err(anyhow::anyhow!(
                                    "Warp session budget of ${:.2} exceeded (spent ${:.2}). \
                                     Restart the bot to reset. Increase [submission].warp_budget_usd if intentional.",
                                    warp_budget_usd, warp_spent_this_session
                                ));
                            }
                            let futures: Vec<_> = submitters.iter()
                                .filter(|s| s.tier() == SubmitTier::AlwaysOn || (s.tier() == SubmitTier::HighEvOnly && use_high_ev))
                                .map(|s| {
                                    let b = bundle.clone();
                                    async move {
                                        metrics::SUBMIT_ATTEMPTS.inc();
                                        metrics::SUBMIT_BY_VENUE.with_label_values(
                                            &[s.venue_name(), if s.tier() == SubmitTier::AlwaysOn { "free" } else { "paid" }]
                                        ).inc();
                                        if s.tier() == SubmitTier::HighEvOnly {
                                            metrics::WARP_SPEND_USD.inc_by(0.15);
                                        }
                                        (s.venue_name(), s.tier(), s.submit(&b).await)
                                    }
                                }).collect();

                            let sub_results = futures::future::join_all(futures).await;
                            let mut any_hash = None;
                            let mut builder_sim_rejected = false;
                            for (venue, tier, result) in sub_results {
                                match result {
                                    Ok(r) if r.success => {
                                        info!(venue, tier = ?tier, hash = ?r.bundle_hash, "Submitted");
                                        if any_hash.is_none() {
                                            if let Some(h) = &r.bundle_hash {
                                                any_hash = h.parse::<B256>().ok();
                                            }
                                        }
                                    }
                                    Ok(r) => {
                                        let err_str = r.error.as_deref().unwrap_or("");
                                        let is_sim_reject = err_str.contains("non-reverting tx in bundle failed")
                                            || err_str.contains("bundle execution failed")
                                            || err_str.contains("transaction execution failed");
                                        if is_sim_reject {
                                            builder_sim_rejected = true;
                                            metrics::BUILDER_SIM_REJECT.inc();
                                        }
                                        debug!(venue, error = ?r.error, "Rejected");
                                    }
                                    Err(e) => warn!(venue, error = %e, "Error"),
                                }
                            }

                            if builder_sim_rejected {
                                circuit_breaker.record_revert(best.path_id, block_number);
                            }

                            // Keep the session Warp spend in sync with the metric
                            if use_high_ev {
                                warp_spent_this_session += 0.15;
                                if warp_spent_this_session >= warp_budget_usd * 0.8 {
                                    warn!(
                                        spent = format!("{:.2}", warp_spent_this_session),
                                        budget = warp_budget_usd,
                                        "Warp spend at 80% of session budget"
                                    );
                                }
                            }

                            // Track receipt — sync in smoke test, async otherwise
                            if let Some(hash) = any_hash {
                                if smoke_test {
                                    info!("Waiting for receipt...");
                                    let outcome = track_tx(endpoint.clone(), hash, 10).await;
                                    match outcome {
                                        TxOutcome::Success => circuit_breaker.record_success(best.path_id),
                                        TxOutcome::Revert => circuit_breaker.record_revert(best.path_id, block_number),
                                        TxOutcome::Dropped => {}
                                    }
                                    let landed_ok = metrics::SUBMIT_LANDED.with_label_values(&["success"]).get() as u64;
                                    let landed_revert = metrics::SUBMIT_LANDED.with_label_values(&["revert"]).get() as u64;
                                    let status = if landed_ok > 0 { "SUCCESS" } else if landed_revert > 0 { "REVERT" } else { "DROPPED" };
                                    println!("\n===== PIPELINE TEST RESULT =====");
                                    println!("chain:            {}", cfg.chain.name);
                                    println!("path_id:          {}", best.path_id);
                                    println!("hops:");
                                    for (i, hop) in optimized_path.hops.iter().enumerate() {
                                        println!("  {}. {:?}  {:.8}..  {} -> {}", i + 1,
                                            hop.protocol, hop.pool, hop.token_in, hop.token_out);
                                    }
                                    println!("flash_amount:     {} ({} token units)", best.flash_amount, best.flash_token);
                                    println!("sim_profit:       {}", best.gross_profit);
                                    println!("sim_profit_bps:   {}", best.profit_bps);
                                    println!("effective_usd:    ${:.4}", effective_usd);
                                    println!("tx_hash:          {hash}");
                                    println!("on_chain_status:  {status}");
                                    println!("gas_spent_wei:    {:.0}", metrics::GAS_SPENT_WEI.get());
                                    println!("=================================\n");
                                    return Ok(());
                                } else {
                                    let ep = endpoint.clone();
                                    let cb_sender = cb_tx.clone();
                                    let pid = best.path_id;
                                    let blk = block_number;
                                    tokio::spawn(async move {
                                        let outcome = track_tx(ep, hash, 5).await;
                                        let _ = cb_sender.send((pid, outcome, blk)).await;
                                    });
                                }
                            }
                        }
                        Err(e) => error!(error = %e, "Failed to build bundle"),
                    }
                } else {
                    info!(path_id = best.path_id, effective_usd = format!("{:.4}", effective_usd),
                        flash_amount = %best.flash_amount, "DRY RUN: would submit");
                }
            }
        }

        // Process pending mempool swaps for backrun opportunities
        while let Ok(pending) = mempool_rx.try_recv() {
            if let (Some(token_in), Some(token_out), Some(_amount_in)) =
                (pending.decoded.token_in, pending.decoded.token_out, pending.decoded.amount_in)
            {
                let affected_paths = token_pair_to_paths.get(&(token_in, token_out));
                if let Some(path_ids) = affected_paths {
                    if path_ids.is_empty() { continue; }
                    metrics::BACKRUN_CANDIDATES.inc();

                    // Project post-swap state for affected pools
                    for &pidx in path_ids.iter().take(20) {
                        let path = &paths[pidx];
                        if circuit_breaker.is_suppressed(path.id, block_number) { continue; }

                        // Evaluate the path with current state (the pending swap hasn't
                        // landed yet — its impact creates a bigger spread for us).
                        // The actual backrun bundle would include the pending tx first.
                        let profit = arb_sim::optimize::find_optimal_amount(
                            path, &store, 
                            flash_bounds.get(&path.flash_token).map(|b| b.0).unwrap_or(path.flash_amount),
                            {
                                let token_max = flash_bounds.get(&path.flash_token).map(|b| b.1)
                                    .unwrap_or(path.flash_amount * U256::from(10u32));
                                let liq_max = arb_sim::optimize::path_max_flash(path, &store, 0.05, token_max);
                                token_max.min(liq_max)
                            },
                            optimization_iterations,
                        );

                        if let Some((opt_amount, opt_profit)) = profit {
                            let profit_bps: u32 = if !opt_amount.is_zero() {
                                ((opt_profit * U256::from(10000u32)) / opt_amount).try_into().unwrap_or(u32::MAX)
                            } else { 0 };

                            let sim = arb_sim::SimResult {
                                path_id: path.id,
                                flash_token: path.flash_token,
                                flash_amount: opt_amount,
                                final_amount: opt_amount + opt_profit,
                                gross_profit: opt_profit,
                                profit_bps,
                            };

                            let decision = profit_gate.should_submit(&sim, path);
                            if decision.accept && !dry_run {
                                info!(
                                    path_id = path.id, profit_bps,
                                    pending_router = pending.decoded.router,
                                    pending_tx = %pending.tx_hash,
                                    "Backrun candidate found"
                                );
                                metrics::BACKRUN_SUBMITTED.inc();

                                // Build and submit as regular (non-bundle) for now.
                                // True 2-tx backrun bundles require target tx raw bytes
                                // which we don't always have from the watcher.
                                let target_block = block_number + 1;
                                if let Ok(bundle) = presign_pool.build_fast(
                                    path.id, opt_amount, &endpoint, arb_contract, &signer, target_block,
                                ).await {
                                    endpoint.bump_nonce();
                                    let futures: Vec<_> = submitters.iter()
                                        .filter(|s| s.tier() == SubmitTier::AlwaysOn)
                                        .map(|s| {
                                            let b = bundle.clone();
                                            async move { (s.venue_name(), s.submit(&b).await) }
                                        }).collect();

                                    let sub_results = futures::future::join_all(futures).await;
                                    for (venue, result) in sub_results {
                                        match result {
                                            Ok(r) if r.success => debug!(venue, "Backrun submitted"),
                                            Ok(r) => debug!(venue, error = ?r.error, "Backrun rejected"),
                                            Err(e) => debug!(venue, error = %e, "Backrun error"),
                                        }
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        let scan_elapsed = scan_start.elapsed();
        metrics::SCAN_LATENCY.observe(scan_elapsed.as_secs_f64());
        let budget_ms = cfg.chain.scan_budget_ms;
        if scan_elapsed.as_millis() as u64 > budget_ms {
            warn!(block = block_number, elapsed_ms = scan_elapsed.as_millis(), budget_ms, "Exceeded budget");
        }

        // Per-minute summary
        if last_summary.elapsed() >= Duration::from_secs(60) {
            let scans_now = metrics::PATHS_EVALUATED.get();
            let submitted_now = metrics::SUBMIT_ATTEMPTS.get();
            let landed_ok = metrics::SUBMIT_LANDED.with_label_values(&["success"]).get() as u64;
            let landed_revert = metrics::SUBMIT_LANDED.with_label_values(&["revert"]).get() as u64;
            let dropped = metrics::SUBMIT_LANDED.with_label_values(&["dropped"]).get() as u64;

            info!(
                block = block_number,
                scans_delta = (scans_now - last_scans) as u64,
                submitted_delta = (submitted_now - last_submitted) as u64,
                landed_ok, landed_revert, dropped,
                paths_suppressed = circuit_breaker.suppressed_count(block_number),
                builder_sim_rejects = metrics::BUILDER_SIM_REJECT.get() as u64,
                warp_usd = format!("{:.2}", metrics::WARP_SPEND_USD.get()),
                "[minute summary]"
            );

            last_scans = scans_now;
            last_submitted = submitted_now;
            last_summary = Instant::now();
        }

        // Status JSON every 5 seconds
        if last_status_write.elapsed() >= Duration::from_secs(5) {
            let balance_str = balance_rx.borrow().clone();
            write_status_json(&chain_name, &started, block_number, &balance_str, &circuit_breaker);
            last_status_write = Instant::now();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_paths::HopTemplate;

    fn addr(b: u8) -> Address {
        Address::with_last_byte(b)
    }

    fn make_path(id: u32, flash: Address, hops: Vec<(Address, Address, Address)>) -> PathTemplate {
        PathTemplate {
            id,
            flash_token: flash,
            flash_amount: U256::from(1000u32),
            hops: hops.into_iter().map(|(pool, tin, tout)| HopTemplate {
                protocol: arb_core::types::Protocol::UniswapV2,
                pool, token_in: tin, token_out: tout,
            }).collect(),
        }
    }

    #[test]
    fn test_token_breaker_skips_flash_token() {
        let flash = addr(1);
        let intermediate = addr(2);
        let target = addr(3);
        let path = make_path(0, flash, vec![
            (addr(10), flash, intermediate),
            (addr(11), intermediate, target),
            (addr(12), target, flash),
        ]);

        let mut breaker = TokenCircuitBreaker::new(2, 100);
        for block in 0..5 {
            breaker.record_revert_for_path(&path, block);
        }

        assert!(!breaker.is_token_suppressed(flash, 5),
            "flash token should never be suppressed");
    }

    #[test]
    fn test_token_breaker_skips_popular_intermediary() {
        let flash = addr(1);
        let weth = addr(2);
        let target = addr(3);
        let path = make_path(0, flash, vec![
            (addr(10), flash, weth),
            (addr(11), weth, target),
            (addr(12), target, flash),
        ]);

        let mut breaker = TokenCircuitBreaker::new(2, 100);
        breaker.set_popular_intermediaries(vec![weth]);

        for block in 0..5 {
            breaker.record_revert_for_path(&path, block);
        }

        assert!(!breaker.is_token_suppressed(weth, 5),
            "popular intermediary should not be suppressed");
        assert!(breaker.is_token_suppressed(target, 5),
            "non-popular target token should be suppressed");
    }

    #[test]
    fn test_token_breaker_suppresses_bad_token() {
        let flash = addr(1);
        let good = addr(2);
        let bad = addr(3);
        // bad appears in 2 positions (token_out of hop2 and token_in of hop3),
        // so each record_revert_for_path call increments its counter by 2.
        let path = make_path(0, flash, vec![
            (addr(10), flash, good),
            (addr(11), good, bad),
            (addr(12), bad, flash),
        ]);

        let mut breaker = TokenCircuitBreaker::new(5, 100);
        breaker.set_popular_intermediaries(vec![good]);

        breaker.record_revert_for_path(&path, 1);
        assert!(!breaker.is_token_suppressed(bad, 2), "2 < 5 threshold");

        breaker.record_revert_for_path(&path, 2);
        assert!(!breaker.is_token_suppressed(bad, 3), "4 < 5 threshold");

        breaker.record_revert_for_path(&path, 3);
        assert!(breaker.is_token_suppressed(bad, 4), "6 >= 5 threshold");
        assert!(!breaker.is_token_suppressed(bad, 104), "expired after suppression window");
    }

    #[test]
    fn test_token_breaker_success_resets() {
        let flash = addr(1);
        let token = addr(2);
        let path = make_path(0, flash, vec![
            (addr(10), flash, token),
            (addr(11), token, flash),
        ]);

        let mut breaker = TokenCircuitBreaker::new(3, 100);
        breaker.record_revert_for_path(&path, 1);
        breaker.record_revert_for_path(&path, 2);
        breaker.record_success_for_path(&path);
        breaker.record_revert_for_path(&path, 3);

        assert!(!breaker.is_token_suppressed(token, 4),
            "success should reset consecutive count");
    }

    #[test]
    fn test_path_circuit_breaker_trip_and_decay() {
        let mut cb = PathCircuitBreaker::new();
        cb.record_submit(1);
        cb.record_revert(1, 100);
        cb.record_revert(1, 101);
        assert!(!cb.is_suppressed(1, 102));

        cb.record_revert(1, 102);
        assert!(cb.is_suppressed(1, 103), "should be suppressed after 3 consecutive reverts");
        assert!(!cb.is_suppressed(1, 200), "should expire after SUPPRESS_BLOCKS");
    }

    #[test]
    fn test_path_circuit_breaker_success_resets() {
        let mut cb = PathCircuitBreaker::new();
        cb.record_revert(1, 1);
        cb.record_revert(1, 2);
        cb.record_success(1);
        cb.record_revert(1, 3);
        assert!(!cb.is_suppressed(1, 4));
    }

    #[test]
    fn test_path_circuit_breaker_decay_gap() {
        let mut cb = PathCircuitBreaker::new();
        cb.record_revert(1, 10);
        cb.record_revert(1, 11);
        cb.record_revert(1, 300);
        assert!(!cb.is_suppressed(1, 301),
            "reverts separated by >DECAY_BLOCKS should reset counter");
    }
}
