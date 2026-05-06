use crate::store::{DiscoveredPool, DiscoveredToken};
use std::collections::HashSet;
use tracing::info;

pub struct FilterConfig {
    pub min_liquidity_usd: f64,
    pub min_volume_usd_24h: f64,
    pub max_buy_tax_bps: u32,
    pub max_sell_tax_bps: u32,
    pub require_audit_passed: bool,
    pub require_social: bool,
    pub major_tokens: HashSet<String>,
}

impl FilterConfig {
    pub fn should_keep_token(&self, token: &DiscoveredToken) -> bool {
        if token.honeypot_status == "yes" { return false; }
        if token.is_flagged { return false; }
        if token.security_level == "high_risk" { return false; }
        if token.buy_tax_bps > self.max_buy_tax_bps { return false; }
        if token.sell_tax_bps > self.max_sell_tax_bps { return false; }
        if token.liquidity_usd < self.min_liquidity_usd { return false; }
        if token.volume_24h_usd < self.min_volume_usd_24h { return false; }
        true
    }

    pub fn should_keep_pool(&self, pool: &DiscoveredPool) -> bool {
        let has_major = self.major_tokens.contains(&pool.token0.to_lowercase())
            || self.major_tokens.contains(&pool.token1.to_lowercase());
        if !has_major { return false; }
        if pool.liquidity_usd < self.min_liquidity_usd { return false; }
        true
    }

    pub fn filter_pools(&self, pools: Vec<DiscoveredPool>) -> Vec<DiscoveredPool> {
        let before = pools.len();
        let kept: Vec<_> = pools.into_iter().filter(|p| self.should_keep_pool(p)).collect();
        info!(before, after = kept.len(), "Pool filter applied");
        kept
    }

    pub fn filter_tokens(&self, tokens: Vec<DiscoveredToken>) -> Vec<DiscoveredToken> {
        let before = tokens.len();
        let kept: Vec<_> = tokens.into_iter().filter(|t| self.should_keep_token(t)).collect();
        info!(before, after = kept.len(), "Token filter applied");
        kept
    }
}
