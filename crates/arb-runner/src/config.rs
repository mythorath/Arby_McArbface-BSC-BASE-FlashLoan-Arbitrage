use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

use arb_core::types::Protocol;
use arb_rpc::ChainConfig;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub chain: ChainConfig,
    pub wallet: WalletConfig,
    pub scanner: ScannerConfig,
    pub submission: SubmissionConfig,
    pub gate: GateConfig,
    pub pools: Vec<PoolEntry>,
    pub tokens: HashMap<String, String>,
    pub token_usd_prices: HashMap<String, f64>,
}

#[derive(Debug, Deserialize)]
pub struct WalletConfig {
    pub private_key_env: String,
}

#[derive(Debug, Deserialize)]
pub struct ScannerConfig {
    pub flash_tokens: Vec<String>,
    pub flash_amounts: HashMap<String, u64>,
    #[serde(default)]
    pub flash_bounds: HashMap<String, FlashBounds>,
    pub min_profit_bps: u32,
    #[serde(default = "default_min_initial_bps")]
    pub min_initial_bps: u32,
    #[serde(default = "default_optimization_iterations")]
    pub optimization_iterations: usize,
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FlashBounds {
    pub min: u64,
    pub max: u64,
}

#[derive(Debug, Deserialize)]
pub struct GateConfig {
    #[serde(default = "default_min_profit_usd")]
    pub min_profit_usd: f64,
    #[serde(default = "default_safety_margin_bps")]
    pub safety_margin_bps: u32,
    #[serde(default = "default_stable_extra_margin")]
    pub stable_pool_extra_margin_bps: u32,
}

#[derive(Debug, Deserialize)]
pub struct SubmissionConfig {
    pub puissant_url: Option<String>,
    pub blockrazor_url: Option<String>,
    pub jetbldr_url: Option<String>,
    pub nodereal_url: Option<String>,
    pub blink_url: Option<String>,
    #[serde(default = "default_warp_threshold")]
    pub warp_threshold_usd: f64,
    #[serde(default = "default_true")]
    pub direct_fallback: bool,
}

fn default_warp_threshold() -> f64 { 1.50 }
fn default_true() -> bool { true }
fn default_min_profit_usd() -> f64 { 0.50 }
fn default_safety_margin_bps() -> u32 { 30 }
fn default_stable_extra_margin() -> u32 { 50 }
fn default_min_initial_bps() -> u32 { 1 }
fn default_optimization_iterations() -> usize { 30 }

#[derive(Debug, Deserialize)]
pub struct PoolEntry {
    #[allow(dead_code)]
    pub name: String,
    pub address: String,
    pub protocol: String,
    pub token0: String,
    pub token1: String,
    pub fee_bps: u32,
}

impl PoolEntry {
    pub fn parse_protocol(&self) -> Protocol {
        match self.protocol.as_str() {
            "v2" | "uniswap_v2" | "pancake_v2" | "biswap" => Protocol::UniswapV2,
            "v3" | "uniswap_v3" | "pancake_v3" => Protocol::UniswapV3,
            "v4" | "uniswap_v4" => Protocol::UniswapV4,
            "pcs_stable" | "curve" => Protocol::PancakeStable,
            "wombat" => Protocol::Wombat,
            "dodo_v2" | "dodo" => Protocol::DodoV2,
            "algebra" | "thena" => Protocol::Algebra,
            "aero_v2" | "aerodrome" | "velodrome" => Protocol::AerodromeV2,
            "aero_slipstream" | "slipstream" => Protocol::AerodromeSlipstream,
            _ => Protocol::UniswapV2,
        }
    }
}

pub fn load_config(path: &str) -> Result<AppConfig> {
    let contents = std::fs::read_to_string(path)?;
    let expanded = expand_env_vars(&contents);
    let config: AppConfig = toml::from_str(&expanded)?;
    Ok(config)
}

fn expand_env_vars(input: &str) -> String {
    let mut result = input.to_string();
    for (key, value) in std::env::vars() {
        result = result.replace(&format!("${{{key}}}"), &value);
        result = result.replace(&format!("${key}"), &value);
    }
    result
}
