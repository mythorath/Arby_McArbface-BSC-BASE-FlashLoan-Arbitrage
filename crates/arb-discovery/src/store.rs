use std::path::{Path, PathBuf};
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
}

impl DiscoveryStore {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
        }
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
        warn!(chain, address, reason, "Added to blacklist");
        Ok(())
    }

    pub fn is_blacklisted(&self, chain: &str, address: &str) -> Result<bool> {
        let blacklist = self.load_blacklist(chain)?;
        let addr_lower = address.to_lowercase();
        Ok(blacklist
            .addresses
            .iter()
            .any(|e| e.address.to_lowercase() == addr_lower))
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
