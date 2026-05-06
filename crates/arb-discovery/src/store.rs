use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use anyhow::Result;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPool {
    pub address: String,
    pub protocol: String,
    pub token0: String,
    pub token1: String,
    pub fee_bps: u32,
    pub exchange_name: String,
    pub liquidity_usd: f64,
    pub volume_24h_usd: f64,
    pub factory_address: String,
    pub locked_lp_rate: f64,
    pub burned_lp_rate: f64,
    pub last_seen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredToken {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub decimals: u32,
    pub price_usd: f64,
    pub market_cap_usd: f64,
    pub liquidity_usd: f64,
    pub volume_24h_usd: f64,
    pub security_level: String,
    pub honeypot_status: String,
    pub buy_tax_bps: u32,
    pub sell_tax_bps: u32,
    pub is_flagged: bool,
    pub holder_count: u64,
    pub top_holder_rate: f64,
    pub source: String,
    pub first_seen: String,
    pub last_seen: String,
    pub platform_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoolUniverse {
    pub chain: String,
    pub pools: Vec<DiscoveredPool>,
    pub last_updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUniverse {
    pub chain: String,
    pub tokens: Vec<DiscoveredToken>,
    pub last_updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Blacklist {
    pub chain: String,
    pub addresses: Vec<BlacklistEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlacklistEntry {
    pub address: String,
    pub reason: String,
    pub added_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreditLedger {
    pub monthly_spend: f64,
    pub daily_spend: f64,
    pub last_reset_month: String,
    pub last_reset_day: String,
    pub total_requests: u64,
}

pub struct DiscoveryStore {
    base_dir: PathBuf,
    blacklist_cache: Mutex<Option<HashSet<String>>>,
}

impl DiscoveryStore {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
            blacklist_cache: Mutex::new(None),
        }
    }

    pub async fn load_pools_async(&self, chain: &str) -> Result<PoolUniverse> {
        let path = self.base_dir.join(format!("pool_universe.{chain}.json"));
        if !path.exists() {
            return Ok(PoolUniverse {
                chain: chain.to_string(),
                ..Default::default()
            });
        }
        let data = tokio::fs::read_to_string(&path).await?;
        let universe: PoolUniverse = serde_json::from_str(&data)?;
        info!(chain, pools = universe.pools.len(), "Loaded pool universe (async)");
        Ok(universe)
    }

    pub async fn load_tokens_async(&self, chain: &str) -> Result<TokenUniverse> {
        let path = self.base_dir.join(format!("token_universe.{chain}.json"));
        if !path.exists() {
            return Ok(TokenUniverse {
                chain: chain.to_string(),
                ..Default::default()
            });
        }
        let data = tokio::fs::read_to_string(&path).await?;
        let universe: TokenUniverse = serde_json::from_str(&data)?;
        info!(chain, tokens = universe.tokens.len(), "Loaded token universe (async)");
        Ok(universe)
    }

    pub fn load_pools(&self, chain: &str) -> Result<PoolUniverse> {
        let path = self.base_dir.join(format!("pool_universe.{chain}.json"));
        if !path.exists() {
            return Ok(PoolUniverse {
                chain: chain.to_string(),
                ..Default::default()
            });
        }
        let data = std::fs::read_to_string(&path)?;
        let universe: PoolUniverse = serde_json::from_str(&data)?;
        info!(chain, pools = universe.pools.len(), "Loaded pool universe");
        Ok(universe)
    }

    pub fn save_pools(&self, chain: &str, universe: &PoolUniverse) -> Result<()> {
        let path = self.base_dir.join(format!("pool_universe.{chain}.json"));
        let tmp_path = self.base_dir.join(format!("pool_universe.{chain}.json.tmp"));
        std::fs::create_dir_all(&self.base_dir)?;
        let data = serde_json::to_string_pretty(universe)?;
        std::fs::write(&tmp_path, &data)?;
        std::fs::rename(&tmp_path, &path)?;
        info!(chain, pools = universe.pools.len(), "Saved pool universe");
        Ok(())
    }

    pub fn load_tokens(&self, chain: &str) -> Result<TokenUniverse> {
        let path = self.base_dir.join(format!("token_universe.{chain}.json"));
        if !path.exists() {
            return Ok(TokenUniverse {
                chain: chain.to_string(),
                ..Default::default()
            });
        }
        let data = std::fs::read_to_string(&path)?;
        let universe: TokenUniverse = serde_json::from_str(&data)?;
        info!(chain, tokens = universe.tokens.len(), "Loaded token universe");
        Ok(universe)
    }

    pub fn save_tokens(&self, chain: &str, universe: &TokenUniverse) -> Result<()> {
        let path = self.base_dir.join(format!("token_universe.{chain}.json"));
        let tmp_path = self.base_dir.join(format!("token_universe.{chain}.json.tmp"));
        std::fs::create_dir_all(&self.base_dir)?;
        let data = serde_json::to_string_pretty(universe)?;
        std::fs::write(&tmp_path, &data)?;
        std::fs::rename(&tmp_path, &path)?;
        info!(chain, tokens = universe.tokens.len(), "Saved token universe");
        Ok(())
    }

    pub fn load_blacklist(&self, chain: &str) -> Result<Blacklist> {
        let path = self.base_dir.join(format!("blacklist.{chain}.json"));
        if !path.exists() {
            return Ok(Blacklist {
                chain: chain.to_string(),
                ..Default::default()
            });
        }
        let data = std::fs::read_to_string(&path)?;
        let blacklist: Blacklist = serde_json::from_str(&data)?;
        info!(chain, entries = blacklist.addresses.len(), "Loaded blacklist");
        Ok(blacklist)
    }

    pub fn add_to_blacklist(&self, chain: &str, address: &str, reason: &str) -> Result<()> {
        let mut blacklist = self.load_blacklist(chain)?;
        let addr_lower = address.to_lowercase();
        if blacklist.addresses.iter().any(|e| e.address.to_lowercase() == addr_lower) {
            return Ok(());
        }
        let entry = BlacklistEntry {
            address: address.to_string(),
            reason: reason.to_string(),
            added_at: chrono::Utc::now().to_rfc3339(),
        };
        blacklist.addresses.push(entry);

        let path = self.base_dir.join(format!("blacklist.{chain}.json"));
        let tmp_path = self.base_dir.join(format!("blacklist.{chain}.json.tmp"));
        std::fs::create_dir_all(&self.base_dir)?;
        let data = serde_json::to_string_pretty(&blacklist)?;
        std::fs::write(&tmp_path, &data)?;
        std::fs::rename(&tmp_path, &path)?;

        if let Ok(mut cache) = self.blacklist_cache.lock() {
            if let Some(ref mut set) = *cache {
                set.insert(addr_lower);
            }
        }
        warn!(chain, address, reason, "Added to blacklist");
        Ok(())
    }

    pub fn is_blacklisted(&self, chain: &str, address: &str) -> Result<bool> {
        let addr_lower = address.to_lowercase();
        let mut cache = self.blacklist_cache.lock().unwrap();
        if cache.is_none() {
            let blacklist = self.load_blacklist(chain)?;
            let set: HashSet<String> = blacklist.addresses.iter()
                .map(|e| e.address.to_lowercase())
                .collect();
            *cache = Some(set);
        }
        Ok(cache.as_ref().unwrap().contains(&addr_lower))
    }

    pub fn prune_stale_pools(&self, chain: &str, max_age_days: i64) -> Result<usize> {
        let mut universe = self.load_pools(chain)?;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days);
        let before = universe.pools.len();
        universe.pools.retain(|p| {
            chrono::DateTime::parse_from_rfc3339(&p.last_seen)
                .map(|dt| dt >= cutoff)
                .unwrap_or(true)
        });
        let pruned = before - universe.pools.len();
        if pruned > 0 {
            universe.last_updated = chrono::Utc::now().to_rfc3339();
            self.save_pools(chain, &universe)?;
            info!(chain, pruned, remaining = universe.pools.len(), "Pruned stale pools");
        }
        Ok(pruned)
    }

    pub fn load_credits(&self) -> Result<CreditLedger> {
        let path = self.base_dir.join("cmc_credits.json");
        if !path.exists() {
            return Ok(CreditLedger::default());
        }
        let data = std::fs::read_to_string(&path)?;
        let ledger: CreditLedger = serde_json::from_str(&data)?;
        Ok(ledger)
    }

    pub fn save_credits(&self, ledger: &CreditLedger) -> Result<()> {
        let path = self.base_dir.join("cmc_credits.json");
        let tmp_path = self.base_dir.join("cmc_credits.json.tmp");
        std::fs::create_dir_all(&self.base_dir)?;
        let data = serde_json::to_string_pretty(ledger)?;
        std::fs::write(&tmp_path, &data)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> (tempfile::TempDir, DiscoveryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::new(dir.path());
        (dir, store)
    }

    fn sample_pool(addr: &str) -> DiscoveredPool {
        DiscoveredPool {
            address: addr.to_string(),
            protocol: "uniswap_v3".into(),
            token0: "0xweth".into(),
            token1: "0xusdc".into(),
            fee_bps: 30,
            exchange_name: "Uniswap V3".into(),
            liquidity_usd: 100_000.0,
            volume_24h_usd: 50_000.0,
            factory_address: "0xfactory".into(),
            locked_lp_rate: 0.0,
            burned_lp_rate: 0.0,
            last_seen: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn sample_token(addr: &str) -> DiscoveredToken {
        DiscoveredToken {
            address: addr.to_string(),
            symbol: "TEST".into(),
            name: "Test Token".into(),
            decimals: 18,
            price_usd: 1.0,
            market_cap_usd: 1_000_000.0,
            liquidity_usd: 200_000.0,
            volume_24h_usd: 80_000.0,
            security_level: "low_risk".into(),
            honeypot_status: "no".into(),
            buy_tax_bps: 0,
            sell_tax_bps: 0,
            is_flagged: false,
            holder_count: 5000,
            top_holder_rate: 0.05,
            source: "cmc".into(),
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: "2026-01-01T00:00:00Z".into(),
            platform_id: 1,
        }
    }

    #[test]
    fn new_store_with_temp_dir() {
        let (dir, store) = make_store();
        assert_eq!(store.base_dir, dir.path());
    }

    #[test]
    fn save_and_load_pools_roundtrip() {
        let (_dir, store) = make_store();
        let universe = PoolUniverse {
            chain: "ethereum".into(),
            pools: vec![sample_pool("0xpool1"), sample_pool("0xpool2")],
            last_updated: "2026-01-01T00:00:00Z".into(),
        };

        store.save_pools("ethereum", &universe).unwrap();
        let loaded = store.load_pools("ethereum").unwrap();

        assert_eq!(loaded.chain, "ethereum");
        assert_eq!(loaded.pools.len(), 2);
        assert_eq!(loaded.pools[0].address, "0xpool1");
        assert_eq!(loaded.pools[1].address, "0xpool2");
        assert_eq!(loaded.pools[0].fee_bps, 30);
    }

    #[test]
    fn load_pools_missing_file_returns_empty() {
        let (_dir, store) = make_store();
        let loaded = store.load_pools("base").unwrap();
        assert_eq!(loaded.chain, "base");
        assert!(loaded.pools.is_empty());
    }

    #[test]
    fn save_and_load_tokens_roundtrip() {
        let (_dir, store) = make_store();
        let universe = TokenUniverse {
            chain: "ethereum".into(),
            tokens: vec![sample_token("0xaaa"), sample_token("0xbbb")],
            last_updated: "2026-01-01T00:00:00Z".into(),
        };

        store.save_tokens("ethereum", &universe).unwrap();
        let loaded = store.load_tokens("ethereum").unwrap();

        assert_eq!(loaded.chain, "ethereum");
        assert_eq!(loaded.tokens.len(), 2);
        assert_eq!(loaded.tokens[0].address, "0xaaa");
        assert_eq!(loaded.tokens[1].symbol, "TEST");
    }

    #[test]
    fn blacklist_add_and_check_case_insensitive() {
        let (_dir, store) = make_store();
        store
            .add_to_blacklist("ethereum", "0xAbCdEf", "scam")
            .unwrap();

        assert!(store.is_blacklisted("ethereum", "0xabcdef").unwrap());
        assert!(store.is_blacklisted("ethereum", "0xABCDEF").unwrap());
        assert!(store.is_blacklisted("ethereum", "0xAbCdEf").unwrap());
    }

    #[test]
    fn is_blacklisted_returns_false_for_unknown() {
        let (_dir, store) = make_store();
        assert!(!store.is_blacklisted("ethereum", "0xnothere").unwrap());
    }

    #[test]
    fn save_and_load_credits_roundtrip() {
        let (_dir, store) = make_store();
        let ledger = CreditLedger {
            monthly_spend: 42.5,
            daily_spend: 3.0,
            last_reset_month: "2026-01".into(),
            last_reset_day: "2026-01-15".into(),
            total_requests: 100,
        };

        store.save_credits(&ledger).unwrap();
        let loaded = store.load_credits().unwrap();

        assert_eq!(loaded.monthly_spend, 42.5);
        assert_eq!(loaded.daily_spend, 3.0);
        assert_eq!(loaded.last_reset_month, "2026-01");
        assert_eq!(loaded.total_requests, 100);
    }

    #[test]
    fn atomic_write_tmp_file_does_not_persist() {
        let (dir, store) = make_store();
        let universe = PoolUniverse {
            chain: "ethereum".into(),
            pools: vec![sample_pool("0x1")],
            last_updated: "2026-01-01T00:00:00Z".into(),
        };

        store.save_pools("ethereum", &universe).unwrap();

        let tmp_path = dir.path().join("pool_universe.ethereum.json.tmp");
        assert!(!tmp_path.exists());

        let final_path = dir.path().join("pool_universe.ethereum.json");
        assert!(final_path.exists());
    }

    #[tokio::test]
    async fn test_async_load_pools() {
        let (_dir, store) = make_store();
        let universe = PoolUniverse {
            chain: "bsc".into(),
            pools: vec![sample_pool("0xpool1")],
            last_updated: "2026-01-01T00:00:00Z".into(),
        };
        store.save_pools("bsc", &universe).unwrap();
        let loaded = store.load_pools_async("bsc").await.unwrap();
        assert_eq!(loaded.pools.len(), 1);
    }

    #[tokio::test]
    async fn test_async_load_tokens() {
        let (_dir, store) = make_store();
        let universe = TokenUniverse {
            chain: "bsc".into(),
            tokens: vec![sample_token("0xt1")],
            last_updated: "2026-01-01T00:00:00Z".into(),
        };
        store.save_tokens("bsc", &universe).unwrap();
        let loaded = store.load_tokens_async("bsc").await.unwrap();
        assert_eq!(loaded.tokens.len(), 1);
    }

    #[tokio::test]
    async fn test_async_load_missing() {
        let (_dir, store) = make_store();
        let loaded = store.load_pools_async("nonexistent").await.unwrap();
        assert!(loaded.pools.is_empty());
    }

    #[test]
    fn test_blacklist_cache_invalidation() {
        let (_dir, store) = make_store();
        assert!(!store.is_blacklisted("bsc", "0xabc").unwrap());
        store.add_to_blacklist("bsc", "0xABC", "test").unwrap();
        assert!(store.is_blacklisted("bsc", "0xabc").unwrap());
    }

    #[test]
    fn test_prune_stale_pools() {
        let (_dir, store) = make_store();
        let old_date = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();
        let recent_date = chrono::Utc::now().to_rfc3339();

        let mut old_pool = sample_pool("0xold");
        old_pool.last_seen = old_date;
        let mut new_pool = sample_pool("0xnew");
        new_pool.last_seen = recent_date;

        let universe = PoolUniverse {
            chain: "bsc".into(),
            pools: vec![old_pool, new_pool],
            last_updated: chrono::Utc::now().to_rfc3339(),
        };
        store.save_pools("bsc", &universe).unwrap();

        let pruned = store.prune_stale_pools("bsc", 30).unwrap();
        assert_eq!(pruned, 1);

        let loaded = store.load_pools("bsc").unwrap();
        assert_eq!(loaded.pools.len(), 1);
        assert_eq!(loaded.pools[0].address, "0xnew");
    }
}
