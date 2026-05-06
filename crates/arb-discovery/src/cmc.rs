use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

const BASE_URL: &str = "https://pro-api.coinmarketcap.com";

// ---------- Rate limiter ----------

struct RateLimiter {
    tokens: Mutex<u32>,
    max_per_minute: u32,
    last_refill: Mutex<Instant>,
}

impl RateLimiter {
    fn new(max_per_minute: u32) -> Self {
        Self {
            tokens: Mutex::new(max_per_minute),
            max_per_minute,
            last_refill: Mutex::new(Instant::now()),
        }
    }

    async fn acquire(&self) {
        loop {
            {
                let mut last = self.last_refill.lock();
                if last.elapsed() >= Duration::from_secs(60) {
                    *self.tokens.lock() = self.max_per_minute;
                    *last = Instant::now();
                }

                let mut t = self.tokens.lock();
                if *t > 0 {
                    *t -= 1;
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

// ---------- Credit accountant ----------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreditLedger {
    pub monthly_spend: f64,
    pub daily_spend: f64,
    pub last_reset_month: String,
    pub last_reset_day: String,
    pub total_requests: u64,
}

struct CreditAccountant {
    monthly_budget: f64,
    spend: AtomicU64, // stored as spend * 100 (fixed-point)
}

impl CreditAccountant {
    fn new(monthly_budget: f64) -> Self {
        Self {
            monthly_budget,
            spend: AtomicU64::new(0),
        }
    }

    fn can_spend(&self) -> bool {
        let current = self.spend.load(Ordering::Relaxed) as f64 / 100.0;
        current < self.monthly_budget
    }

    fn record(&self, credits: f64) {
        let delta = (credits * 100.0) as u64;
        self.spend.fetch_add(delta, Ordering::Relaxed);
    }

    fn total(&self) -> f64 {
        self.spend.load(Ordering::Relaxed) as f64 / 100.0
    }
}

// ---------- CMC response types ----------

#[derive(Debug, Deserialize)]
pub struct CmcStatus {
    pub error_code: Option<i32>,
    pub error_message: Option<String>,
    pub credit_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CmcResponse<T> {
    pub status: Option<CmcStatus>,
    pub data: Option<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenLeaderboardEntry {
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub sym: Option<String>,
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub plt: Option<String>,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub pcid: Option<i32>,
    #[serde(default)]
    pub fdv: Option<String>,
    #[serde(default)]
    pub mcap: Option<String>,
    #[serde(default, rename = "liqUsd")]
    pub liq_usd: Option<String>,
    #[serde(default)]
    pub p: Option<String>,
    #[serde(default)]
    pub v24h: Option<String>,
    #[serde(default)]
    pub ch24h: Option<String>,
    #[serde(default)]
    pub thr: Option<String>,
    #[serde(default)]
    pub dec: Option<i32>,
    #[serde(default)]
    pub rl: Option<String>,
    #[serde(default)]
    pub hld: Option<i32>,
    #[serde(default)]
    pub hcnt: Option<i32>,
    #[serde(default)]
    pub tsrc: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewTokenResponse {
    #[serde(default, rename = "leaderboardList")]
    pub leaderboard_list: Option<Vec<TokenLeaderboardEntry>>,
    pub total: Option<i64>,
    #[serde(default, rename = "hasNextPage")]
    pub has_next_page: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemeTokenEntry {
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub sym: Option<String>,
    #[serde(default)]
    pub dec: Option<i32>,
    #[serde(default)]
    pub mcap: Option<serde_json::Value>,
    #[serde(default)]
    pub liq: Option<serde_json::Value>,
    #[serde(default)]
    pub vu: Option<serde_json::Value>,
    #[serde(default)]
    pub p: Option<serde_json::Value>,
    #[serde(default)]
    pub bc: Option<serde_json::Value>,
    #[serde(default)]
    pub htp: Option<f64>,
    #[serde(default)]
    pub h: Option<i64>,
    #[serde(default)]
    pub plt: Option<i32>,
    #[serde(default)]
    pub pn: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MemeResponse {
    pub graduates: Option<MemeResultList>,
    #[serde(rename = "aboutGraduates")]
    pub about_graduates: Option<MemeResultList>,
    #[serde(rename = "newCreations")]
    pub new_creations: Option<MemeResultList>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MemeResultList {
    Single(MemeTokenEntry),
    List(Vec<MemeTokenEntry>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolEntry {
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub v24: Option<String>,
    #[serde(default, rename = "liqUsd")]
    pub liq_usd: Option<String>,
    #[serde(default)]
    pub exn: Option<String>,
    #[serde(default)]
    pub fa: Option<String>,
    #[serde(default)]
    pub lr: Option<String>,
    #[serde(default)]
    pub br: Option<String>,
    #[serde(default)]
    pub t0: Option<PoolToken>,
    #[serde(default)]
    pub t1: Option<PoolToken>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolToken {
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub sym: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default, rename = "liqUsd")]
    pub liq_usd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityResponse {
    #[serde(default, rename = "securityLevel")]
    pub security_level: Option<String>,
    #[serde(default)]
    pub extra: Option<SecurityExtra>,
    #[serde(default)]
    pub exist: Option<bool>,
    #[serde(default, rename = "evmDisplay")]
    pub evm_display: Option<EvmDisplay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityExtra {
    #[serde(default, rename = "buyTax")]
    pub buy_tax: Option<String>,
    #[serde(default, rename = "sellTax")]
    pub sell_tax: Option<String>,
    #[serde(default, rename = "isFlaggedByVendor")]
    pub is_flagged_by_vendor: Option<bool>,
    #[serde(default, rename = "isVerified")]
    pub is_verified: Option<bool>,
    #[serde(default, rename = "isReported")]
    pub is_reported: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvmDisplay {
    #[serde(default, rename = "honeypotStatus")]
    pub honeypot_status: Option<String>,
    #[serde(default, rename = "unverifiedContractStatus")]
    pub unverified_contract_status: Option<String>,
}

// ---------- Standard API types ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingEntry {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub platform: Option<ListingPlatform>,
    pub quote: Option<std::collections::HashMap<String, ListingQuote>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingPlatform {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub token_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingQuote {
    pub price: Option<f64>,
    pub volume_24h: Option<f64>,
    pub percent_change_24h: Option<f64>,
    pub market_cap: Option<f64>,
}

// ---------- Client ----------

pub struct CmcClient {
    http: reqwest::Client,
    rate_limiter: Arc<RateLimiter>,
    accountant: Arc<CreditAccountant>,
}

impl CmcClient {
    pub fn new(api_key: &str, per_minute_cap: u32, monthly_budget: f64) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-CMC_PRO_API_KEY",
            HeaderValue::from_str(api_key).context("invalid API key")?,
        );
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Self {
            http,
            rate_limiter: Arc::new(RateLimiter::new(per_minute_cap)),
            accountant: Arc::new(CreditAccountant::new(monthly_budget)),
        })
    }

    pub fn credit_spend(&self) -> f64 {
        self.accountant.total()
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        if !self.accountant.can_spend() {
            anyhow::bail!("CMC monthly credit budget exceeded");
        }
        self.rate_limiter.acquire().await;

        let url = format!("{BASE_URL}{path}");
        debug!(url = %url, "CMC GET");

        let mut backoff = Duration::from_millis(500);
        for attempt in 0..3 {
            let resp = self.http.get(&url).send().await?;
            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                warn!(attempt, "CMC 429 rate limited, backing off");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            if status.is_server_error() {
                warn!(attempt, status = %status, "CMC 5xx error, retrying");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            self.accountant.record(1.0);
            let body = resp.text().await?;
            let parsed: T = serde_json::from_str(&body)
                .with_context(|| format!("Failed to parse CMC response: {}", &body[..200.min(body.len())]))?;
            return Ok(parsed);
        }

        anyhow::bail!("CMC request failed after 3 attempts: {url}")
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &serde_json::Value) -> Result<T> {
        if !self.accountant.can_spend() {
            anyhow::bail!("CMC monthly credit budget exceeded");
        }
        self.rate_limiter.acquire().await;

        let url = format!("{BASE_URL}{path}");
        debug!(url = %url, "CMC POST");

        let mut backoff = Duration::from_millis(500);
        for attempt in 0..3 {
            let resp = self.http.post(&url).json(body).send().await?;
            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                warn!(attempt, "CMC 429 rate limited, backing off");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            if status.is_server_error() {
                warn!(attempt, status = %status, "CMC 5xx error, retrying");
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }

            self.accountant.record(1.0);
            let body_text = resp.text().await?;
            let parsed: T = serde_json::from_str(&body_text)
                .with_context(|| format!("Failed to parse CMC response: {}", &body_text[..200.min(body_text.len())]))?;
            return Ok(parsed);
        }

        anyhow::bail!("CMC POST failed after 3 attempts: {url}")
    }

    // ---------- API wrappers ----------

    pub async fn dex_new_list(
        &self,
        platform_ids: &str,
        min_liquidity: f64,
        max_age_minutes: u32,
    ) -> Result<Vec<TokenLeaderboardEntry>> {
        let body = serde_json::json!({
            "platformIds": platform_ids,
            "filter": {
                "minLiquidity": min_liquidity,
                "maxAge": max_age_minutes,
                "auditPassed": true,
                "social": true,
            },
            "pageSize": 100,
            "sortBy": "liqUsd",
            "sortType": "desc",
        });

        let resp: CmcResponse<NewTokenResponse> = self.post("/v1/dex/new/list", &body).await?;
        Ok(resp.data
            .and_then(|d| d.leaderboard_list)
            .unwrap_or_default())
    }

    pub async fn dex_meme_list(&self, platform_id: i32) -> Result<Vec<MemeTokenEntry>> {
        let body = serde_json::json!({
            "protocol": platform_id,
            "graduateFilter": {
                "minLiquidity": 25000.0,
                "minVolume": 10000.0,
            },
            "limit": 50,
        });

        let resp: CmcResponse<MemeResponse> = self.post("/v1/dex/meme/list", &body).await?;
        let mut tokens = Vec::new();
        if let Some(data) = resp.data {
            if let Some(grads) = data.graduates {
                match grads {
                    MemeResultList::Single(t) => tokens.push(t),
                    MemeResultList::List(list) => tokens.extend(list),
                }
            }
        }
        Ok(tokens)
    }

    pub async fn dex_gainer_loser_list(&self, platform_ids: &str) -> Result<Vec<TokenLeaderboardEntry>> {
        let body = serde_json::json!({
            "platformIds": platform_ids,
            "filter": {
                "minLiquidity": 25000.0,
            },
            "pageSize": 50,
            "sortBy": "ch24h",
            "sortType": "desc",
        });

        let resp: CmcResponse<NewTokenResponse> = self.post("/v1/dex/gainer-loser/list", &body).await?;
        Ok(resp.data
            .and_then(|d| d.leaderboard_list)
            .unwrap_or_default())
    }

    pub async fn dex_token_pools(
        &self,
        platform: &str,
        address: &str,
    ) -> Result<Vec<PoolEntry>> {
        let path = format!(
            "/v1/dex/token/pools?platform={}&address={}&size=20",
            platform, address,
        );
        let resp: CmcResponse<Vec<PoolEntry>> = self.get(&path).await?;
        Ok(resp.data.unwrap_or_default())
    }

    pub async fn dex_security_detail(
        &self,
        platform: &str,
        address: &str,
    ) -> Result<SecurityResponse> {
        let path = format!(
            "/v1/dex/security/detail?platformName={}&address={}",
            platform, address,
        );
        // The response may be an array with one element or direct object
        let resp: CmcResponse<serde_json::Value> = self.get(&path).await?;
        if let Some(data) = resp.data {
            if let Some(arr) = data.as_array() {
                if let Some(first) = arr.first() {
                    return Ok(serde_json::from_value(first.clone())?);
                }
            }
            return Ok(serde_json::from_value(data)?);
        }
        Ok(SecurityResponse {
            security_level: None,
            extra: None,
            exist: Some(false),
            evm_display: None,
        })
    }

    pub async fn dex_platform_list(&self) -> Result<serde_json::Value> {
        self.get("/v1/dex/platform/list").await
    }

    // ---- Standard API endpoints (available on Hobbyist tier) ----

    pub async fn listings_newest(
        &self,
        limit: u32,
        volume_min: f64,
    ) -> Result<Vec<ListingEntry>> {
        let path = format!(
            "/v1/cryptocurrency/listings/latest?limit={}&sort=date_added&sort_dir=desc&cryptocurrency_type=tokens&volume_24h_min={}&convert=USD",
            limit, volume_min,
        );
        let resp: CmcResponse<Vec<ListingEntry>> = self.get(&path).await?;
        Ok(resp.data.unwrap_or_default())
    }

    pub async fn listings_gainers(
        &self,
        limit: u32,
        volume_min: f64,
    ) -> Result<Vec<ListingEntry>> {
        let path = format!(
            "/v1/cryptocurrency/listings/latest?limit={}&sort=percent_change_24h&sort_dir=desc&cryptocurrency_type=tokens&volume_24h_min={}&convert=USD",
            limit, volume_min,
        );
        let resp: CmcResponse<Vec<ListingEntry>> = self.get(&path).await?;
        Ok(resp.data.unwrap_or_default())
    }

    pub async fn key_info(&self) -> Result<serde_json::Value> {
        self.get("/v1/key/info").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credit_accountant_can_spend_under_budget() {
        let acc = CreditAccountant::new(100.0);
        assert!(acc.can_spend());
    }

    #[test]
    fn credit_accountant_can_spend_over_budget() {
        let acc = CreditAccountant::new(10.0);
        acc.record(10.01);
        assert!(!acc.can_spend());
    }

    #[test]
    fn credit_accountant_record_accumulates() {
        let acc = CreditAccountant::new(1000.0);
        acc.record(3.5);
        acc.record(2.25);
        acc.record(4.25);
        assert!((acc.total() - 10.0).abs() < 0.02);
    }

    #[test]
    fn credit_accountant_total_initially_zero() {
        let acc = CreditAccountant::new(50.0);
        assert_eq!(acc.total(), 0.0);
    }

    #[test]
    fn deser_token_leaderboard_all_fields() {
        let json = r#"{
            "n": "TestToken",
            "sym": "TT",
            "addr": "0xabc",
            "plt": "ethereum",
            "pid": 1,
            "pcid": 2,
            "fdv": "1000000",
            "mcap": "500000",
            "liqUsd": "250000",
            "p": "1.23",
            "v24h": "100000",
            "ch24h": "5.5",
            "thr": "0.1",
            "dec": 18,
            "rl": "low",
            "hld": 500,
            "hcnt": 1000,
            "tsrc": "cmc"
        }"#;
        let entry: TokenLeaderboardEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.n.as_deref(), Some("TestToken"));
        assert_eq!(entry.sym.as_deref(), Some("TT"));
        assert_eq!(entry.addr.as_deref(), Some("0xabc"));
        assert_eq!(entry.pid, Some(1));
        assert_eq!(entry.dec, Some(18));
        assert_eq!(entry.hld, Some(500));
        assert_eq!(entry.liq_usd.as_deref(), Some("250000"));
    }

    #[test]
    fn deser_token_leaderboard_missing_optional_fields() {
        let json = r#"{"n": "Minimal"}"#;
        let entry: TokenLeaderboardEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.n.as_deref(), Some("Minimal"));
        assert!(entry.sym.is_none());
        assert!(entry.addr.is_none());
        assert!(entry.pid.is_none());
        assert!(entry.dec.is_none());
        assert!(entry.liq_usd.is_none());
        assert!(entry.v24h.is_none());
    }

    #[test]
    fn deser_meme_token_entry() {
        let json = r#"{
            "addr": "0xmeme",
            "n": "MemeCoin",
            "sym": "MEME",
            "dec": 9,
            "mcap": 42000.0,
            "liq": "15000",
            "vu": null,
            "p": "0.001",
            "htp": 0.15,
            "h": 300,
            "plt": 1,
            "pn": "pump.fun"
        }"#;
        let entry: MemeTokenEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.addr.as_deref(), Some("0xmeme"));
        assert_eq!(entry.sym.as_deref(), Some("MEME"));
        assert_eq!(entry.dec, Some(9));
        assert_eq!(entry.htp, Some(0.15));
        assert_eq!(entry.h, Some(300));
        assert_eq!(entry.plt, Some(1));
    }

    #[test]
    fn deser_security_response() {
        let json = r#"{
            "securityLevel": "high_risk",
            "extra": {
                "buyTax": "500",
                "sellTax": "1000",
                "isFlaggedByVendor": true,
                "isVerified": false,
                "isReported": true
            },
            "exist": true,
            "evmDisplay": {
                "honeypotStatus": "yes",
                "unverifiedContractStatus": "unverified"
            }
        }"#;
        let resp: SecurityResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.security_level.as_deref(), Some("high_risk"));
        assert_eq!(resp.exist, Some(true));

        let extra = resp.extra.unwrap();
        assert_eq!(extra.buy_tax.as_deref(), Some("500"));
        assert_eq!(extra.sell_tax.as_deref(), Some("1000"));
        assert_eq!(extra.is_flagged_by_vendor, Some(true));

        let evm = resp.evm_display.unwrap();
        assert_eq!(evm.honeypot_status.as_deref(), Some("yes"));
    }

    #[test]
    fn deser_meme_result_list_single() {
        let json = r#"{"addr": "0xsingle", "n": "One"}"#;
        let result: MemeResultList = serde_json::from_str(json).unwrap();
        match result {
            MemeResultList::Single(entry) => {
                assert_eq!(entry.addr.as_deref(), Some("0xsingle"));
            }
            MemeResultList::List(_) => panic!("expected Single variant"),
        }
    }

    #[test]
    fn deser_meme_result_list_vec() {
        let json = r#"[
            {"addr": "0xa", "n": "A"},
            {"addr": "0xb", "n": "B"}
        ]"#;
        let result: MemeResultList = serde_json::from_str(json).unwrap();
        match result {
            MemeResultList::List(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].addr.as_deref(), Some("0xa"));
                assert_eq!(entries[1].addr.as_deref(), Some("0xb"));
            }
            MemeResultList::Single(_) => panic!("expected List variant"),
        }
    }

    #[test]
    fn deser_pool_entry_with_tokens() {
        let json = r#"{
            "addr": "0xpool",
            "v24": "50000",
            "liqUsd": "100000",
            "exn": "uniswap_v3",
            "fa": "0xfactory",
            "lr": "0.5",
            "br": "0.1",
            "t0": {
                "addr": "0xweth",
                "sym": "WETH",
                "n": "Wrapped Ether",
                "liqUsd": "50000"
            },
            "t1": {
                "addr": "0xusdc",
                "sym": "USDC",
                "n": "USD Coin",
                "liqUsd": "50000"
            }
        }"#;
        let pool: PoolEntry = serde_json::from_str(json).unwrap();
        assert_eq!(pool.addr.as_deref(), Some("0xpool"));
        assert_eq!(pool.liq_usd.as_deref(), Some("100000"));
        assert_eq!(pool.exn.as_deref(), Some("uniswap_v3"));

        let t0 = pool.t0.unwrap();
        assert_eq!(t0.sym.as_deref(), Some("WETH"));
        assert_eq!(t0.liq_usd.as_deref(), Some("50000"));

        let t1 = pool.t1.unwrap();
        assert_eq!(t1.sym.as_deref(), Some("USDC"));
    }
}
