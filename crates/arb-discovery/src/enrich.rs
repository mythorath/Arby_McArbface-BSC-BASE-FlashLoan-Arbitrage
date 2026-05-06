use anyhow::Result;
use tracing::{debug, info};

use crate::cmc::CmcClient;
use crate::feeds::FeedCandidate;
use crate::store::{DiscoveredPool, DiscoveredToken, DiscoveryStore};

pub struct EnrichConfig {
    pub max_buy_tax_bps: u32,
    pub max_sell_tax_bps: u32,
    pub platform_name: String,
    pub chain_name: String,
}

fn parse_tax_bps(s: &Option<String>) -> u32 {
    s.as_ref()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|pct| (pct.max(0.0) * 100.0) as u32)
        .unwrap_or(0)
}

/// Enrich a candidate token with pool inventory and security data from CMC.
/// Returns the enriched token + its discovered pools, or None if rejected.
pub async fn enrich_candidate(
    client: &CmcClient,
    candidate: &FeedCandidate,
    config: &EnrichConfig,
    store: &DiscoveryStore,
) -> Result<Option<(DiscoveredToken, Vec<DiscoveredPool>)>> {
    let addr = &candidate.address;

    if store.is_blacklisted(&config.chain_name, addr)? {
        debug!(token = %addr, "Skipping blacklisted token");
        return Ok(None);
    }

    // 1. Security check
    let security = client
        .dex_security_detail(&config.platform_name, addr)
        .await?;

    let honeypot = security
        .evm_display
        .as_ref()
        .and_then(|d| d.honeypot_status.clone())
        .unwrap_or_default();

    if honeypot.to_lowercase() == "yes" {
        debug!(token = %addr, "Rejected: honeypot");
        return Ok(None);
    }

    let is_flagged = security
        .extra
        .as_ref()
        .and_then(|e| e.is_flagged_by_vendor)
        .unwrap_or(false);

    if is_flagged {
        debug!(token = %addr, "Rejected: flagged by vendor");
        return Ok(None);
    }

    let sec_level = security.security_level.clone().unwrap_or_default();
    if sec_level == "high_risk" {
        debug!(token = %addr, "Rejected: high_risk security level");
        return Ok(None);
    }

    let buy_tax = parse_tax_bps(&security.extra.as_ref().and_then(|e| e.buy_tax.clone()));
    let sell_tax = parse_tax_bps(&security.extra.as_ref().and_then(|e| e.sell_tax.clone()));

    if buy_tax > config.max_buy_tax_bps {
        debug!(token = %addr, buy_tax, max = config.max_buy_tax_bps, "Rejected: high buy tax");
        return Ok(None);
    }
    if sell_tax > config.max_sell_tax_bps {
        debug!(token = %addr, sell_tax, max = config.max_sell_tax_bps, "Rejected: high sell tax");
        return Ok(None);
    }

    // 2. Pool inventory
    let pools_raw = client
        .dex_token_pools(&config.platform_name, addr)
        .await?;

    let now = chrono::Utc::now().to_rfc3339();
    let pools: Vec<DiscoveredPool> = pools_raw
        .iter()
        .filter_map(|p| {
            let pool_addr = p.addr.as_ref()?;
            let t0 = p.t0.as_ref()?;
            let t1 = p.t1.as_ref()?;

            let liq: f64 = p.liq_usd.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let vol: f64 = p.v24.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0);

            let exchange = p.exn.clone().unwrap_or_default();
            let protocol = exchange_to_protocol(&exchange);

            Some(DiscoveredPool {
                address: pool_addr.clone(),
                protocol,
                token0: t0.addr.clone().unwrap_or_default(),
                token1: t1.addr.clone().unwrap_or_default(),
                fee_bps: guess_fee_bps(&exchange),
                exchange_name: exchange,
                liquidity_usd: liq,
                volume_24h_usd: vol,
                factory_address: p.fa.clone().unwrap_or_default(),
                locked_lp_rate: p.lr.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
                burned_lp_rate: p.br.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0.0),
                last_seen: now.clone(),
            })
        })
        .collect();

    // 3. Build enriched token
    let mut token = candidate.token.clone().unwrap_or_else(|| DiscoveredToken {
        address: addr.clone(),
        symbol: String::new(),
        name: String::new(),
        decimals: 18,
        price_usd: 0.0,
        market_cap_usd: 0.0,
        liquidity_usd: 0.0,
        volume_24h_usd: 0.0,
        security_level: String::new(),
        honeypot_status: String::new(),
        buy_tax_bps: 0,
        sell_tax_bps: 0,
        is_flagged: false,
        holder_count: 0,
        top_holder_rate: 0.0,
        source: candidate.source.clone(),
        first_seen: now.clone(),
        last_seen: now.clone(),
        platform_id: 0,
    });

    token.security_level = sec_level;
    token.honeypot_status = honeypot;
    token.buy_tax_bps = buy_tax;
    token.sell_tax_bps = sell_tax;
    token.is_flagged = is_flagged;
    token.last_seen = now;

    info!(
        token = %addr,
        symbol = %token.symbol,
        pools = pools.len(),
        buy_tax = buy_tax,
        sell_tax = sell_tax,
        security = %token.security_level,
        "Token enriched"
    );

    Ok(Some((token, pools)))
}

fn exchange_to_protocol(exchange: &str) -> String {
    let lower = exchange.to_lowercase();
    if lower.contains("slipstream") || lower.contains("aerodrome cl") {
        "aero_slipstream".to_string()
    } else if lower.contains("v3") {
        "v3".to_string()
    } else if lower.contains("aerodrome") || lower.contains("velodrome") {
        "aero_v2".to_string()
    } else if lower.contains("algebra") || lower.contains("thena") {
        "algebra".to_string()
    } else if lower.contains("v2") || lower.contains("swap") {
        "v2".to_string()
    } else {
        "v2".to_string()
    }
}

fn guess_fee_bps(exchange: &str) -> u32 {
    let lower = exchange.to_lowercase();
    if lower.contains("v3") || lower.contains("slipstream") || lower.contains("algebra") {
        30
    } else if lower.contains("biswap") {
        10
    } else if lower.contains("aerodrome") || lower.contains("velodrome") {
        30
    } else {
        25
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tax_bps_whole_number() {
        assert_eq!(parse_tax_bps(&Some("5".to_string())), 500);
    }

    #[test]
    fn parse_tax_bps_zero() {
        assert_eq!(parse_tax_bps(&Some("0".to_string())), 0);
    }

    #[test]
    fn parse_tax_bps_fractional() {
        assert_eq!(parse_tax_bps(&Some("0.5".to_string())), 50);
    }

    #[test]
    fn parse_tax_bps_none() {
        assert_eq!(parse_tax_bps(&None), 0);
    }

    #[test]
    fn parse_tax_bps_invalid_string() {
        assert_eq!(parse_tax_bps(&Some("not_a_number".to_string())), 0);
    }

    #[test]
    fn parse_tax_bps_hundred_percent() {
        assert_eq!(parse_tax_bps(&Some("100".to_string())), 10000);
    }

    #[test]
    fn exchange_to_protocol_pancakeswap_v3() {
        assert_eq!(exchange_to_protocol("PancakeSwap V3"), "v3");
    }

    #[test]
    fn exchange_to_protocol_uniswap_v3() {
        assert_eq!(exchange_to_protocol("Uniswap V3"), "v3");
    }

    #[test]
    fn exchange_to_protocol_pancakeswap_v2() {
        assert_eq!(exchange_to_protocol("PancakeSwap V2"), "v2");
    }

    #[test]
    fn exchange_to_protocol_biswap() {
        assert_eq!(exchange_to_protocol("BiSwap"), "v2");
    }

    #[test]
    fn exchange_to_protocol_aerodrome_v2() {
        assert_eq!(exchange_to_protocol("Aerodrome V2"), "aero_v2");
    }

    #[test]
    fn exchange_to_protocol_velodrome_v2() {
        assert_eq!(exchange_to_protocol("Velodrome V2"), "aero_v2");
    }

    #[test]
    fn exchange_to_protocol_aerodrome_slipstream() {
        assert_eq!(exchange_to_protocol("Aerodrome Slipstream"), "aero_slipstream");
    }

    #[test]
    fn exchange_to_protocol_aerodrome_cl() {
        assert_eq!(exchange_to_protocol("Aerodrome CL"), "aero_slipstream");
    }

    #[test]
    fn exchange_to_protocol_thena_fusion() {
        assert_eq!(exchange_to_protocol("Thena Fusion"), "algebra");
    }

    #[test]
    fn exchange_to_protocol_unknown_dex() {
        assert_eq!(exchange_to_protocol("SomeUnknownDex"), "v2");
    }

    #[test]
    fn guess_fee_bps_v3() {
        assert_eq!(guess_fee_bps("PancakeSwap V3"), 30);
    }

    #[test]
    fn guess_fee_bps_biswap() {
        assert_eq!(guess_fee_bps("BiSwap V2"), 10);
    }

    #[test]
    fn guess_fee_bps_aerodrome() {
        assert_eq!(guess_fee_bps("Aerodrome V2"), 30);
    }

    #[test]
    fn guess_fee_bps_velodrome() {
        assert_eq!(guess_fee_bps("Velodrome V2"), 30);
    }

    #[test]
    fn guess_fee_bps_default() {
        assert_eq!(guess_fee_bps("PancakeSwap V2"), 25);
    }
}
