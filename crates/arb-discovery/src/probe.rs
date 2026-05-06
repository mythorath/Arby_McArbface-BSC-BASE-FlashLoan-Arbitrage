use alloy_primitives::Address;
use anyhow::Result;
use tracing::warn;

pub struct ProbeConfig {
    pub skip_onchain_probe: bool,
}

pub struct ProbeResult {
    pub address: Address,
    pub decimals: Option<u32>,
    pub symbol: Option<String>,
    pub name: Option<String>,
    pub passed: bool,
    pub reject_reason: Option<String>,
}

/// Placeholder for on-chain probing. When `skip_onchain_probe` is true, all tokens
/// that passed CMC pre-screening are assumed safe. When false, we'd do a tiny swap
/// probe to catch honeypots that defeated CMC's static analysis.
pub async fn probe_token(
    _config: &ProbeConfig,
    token_address: Address,
    _rpc_url: &str,
) -> Result<ProbeResult> {
    Ok(ProbeResult {
        address: token_address,
        decimals: None,
        symbol: None,
        name: None,
        passed: true,
        reject_reason: None,
    })
}

/// Batch-probe multiple tokens. Returns results for each.
pub async fn probe_tokens(
    config: &ProbeConfig,
    tokens: &[Address],
    rpc_url: &str,
) -> Vec<ProbeResult> {
    let mut results = Vec::with_capacity(tokens.len());
    for &token in tokens {
        match probe_token(config, token, rpc_url).await {
            Ok(r) => results.push(r),
            Err(e) => {
                warn!(token = %token, error = %e, "Probe failed");
                results.push(ProbeResult {
                    address: token,
                    decimals: None,
                    symbol: None,
                    name: None,
                    passed: false,
                    reject_reason: Some(format!("probe error: {e}")),
                });
            }
        }
    }
    results
}
