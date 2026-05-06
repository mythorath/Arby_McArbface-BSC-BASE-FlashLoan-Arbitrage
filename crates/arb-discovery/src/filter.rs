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

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> FilterConfig {
        let mut major = HashSet::new();
        major.insert("0xweth".to_string());
        major.insert("0xusdc".to_string());
        FilterConfig {
            min_liquidity_usd: 25_000.0,
            min_volume_usd_24h: 10_000.0,
            max_buy_tax_bps: 500,
            max_sell_tax_bps: 500,
            require_audit_passed: false,
            require_social: false,
            major_tokens: major,
        }
    }

    fn valid_token() -> DiscoveredToken {
        DiscoveredToken {
            address: "0xtoken".into(),
            symbol: "TKN".into(),
            name: "Token".into(),
            decimals: 18,
            price_usd: 1.0,
            market_cap_usd: 500_000.0,
            liquidity_usd: 100_000.0,
            volume_24h_usd: 50_000.0,
            security_level: "low_risk".into(),
            honeypot_status: "no".into(),
            buy_tax_bps: 100,
            sell_tax_bps: 100,
            is_flagged: false,
            holder_count: 2000,
            top_holder_rate: 0.05,
            source: "cmc".into(),
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: "2026-01-01T00:00:00Z".into(),
            platform_id: 1,
        }
    }

    fn valid_pool() -> DiscoveredPool {
        DiscoveredPool {
            address: "0xpool".into(),
            protocol: "uniswap_v3".into(),
            token0: "0xweth".into(),
            token1: "0xtoken".into(),
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

    #[test]
    fn rejects_honeypot_token() {
        let cfg = default_config();
        let mut t = valid_token();
        t.honeypot_status = "yes".into();
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn rejects_flagged_token() {
        let cfg = default_config();
        let mut t = valid_token();
        t.is_flagged = true;
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn rejects_high_risk_security_level() {
        let cfg = default_config();
        let mut t = valid_token();
        t.security_level = "high_risk".into();
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn rejects_excessive_buy_tax() {
        let cfg = default_config();
        let mut t = valid_token();
        t.buy_tax_bps = 501;
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn rejects_excessive_sell_tax() {
        let cfg = default_config();
        let mut t = valid_token();
        t.sell_tax_bps = 501;
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn rejects_low_liquidity_token() {
        let cfg = default_config();
        let mut t = valid_token();
        t.liquidity_usd = 24_999.0;
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn rejects_low_volume_token() {
        let cfg = default_config();
        let mut t = valid_token();
        t.volume_24h_usd = 9_999.0;
        assert!(!cfg.should_keep_token(&t));
    }

    #[test]
    fn accepts_valid_token() {
        let cfg = default_config();
        assert!(cfg.should_keep_token(&valid_token()));
    }

    #[test]
    fn rejects_pool_without_major_token() {
        let cfg = default_config();
        let mut p = valid_pool();
        p.token0 = "0xrandom1".into();
        p.token1 = "0xrandom2".into();
        assert!(!cfg.should_keep_pool(&p));
    }

    #[test]
    fn rejects_pool_below_liquidity_threshold() {
        let cfg = default_config();
        let mut p = valid_pool();
        p.liquidity_usd = 24_999.0;
        assert!(!cfg.should_keep_pool(&p));
    }

    #[test]
    fn accepts_valid_pool_with_major_token() {
        let cfg = default_config();
        assert!(cfg.should_keep_pool(&valid_pool()));
    }

    #[test]
    fn filter_pools_applies_correctly() {
        let cfg = default_config();
        let good = valid_pool();
        let mut bad_liq = valid_pool();
        bad_liq.liquidity_usd = 1_000.0;
        let mut bad_tokens = valid_pool();
        bad_tokens.token0 = "0xjunk".into();
        bad_tokens.token1 = "0xjunk2".into();

        let result = cfg.filter_pools(vec![good, bad_liq, bad_tokens]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].address, "0xpool");
    }

    #[test]
    fn filter_tokens_applies_correctly() {
        let cfg = default_config();
        let good = valid_token();
        let mut honeypot = valid_token();
        honeypot.honeypot_status = "yes".into();
        let mut low_vol = valid_token();
        low_vol.volume_24h_usd = 1.0;

        let result = cfg.filter_tokens(vec![good, honeypot, low_vol]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].address, "0xtoken");
    }
}
