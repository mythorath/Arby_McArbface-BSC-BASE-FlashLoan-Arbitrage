use std::collections::HashMap;

use alloy_primitives::{Address, U256};

use arb_core::types::Protocol;
use arb_paths::PathTemplate;

use crate::SimResult;

#[derive(Debug, Clone)]
pub struct Decision {
    pub accept: bool,
    pub effective_profit_usd: f64,
    pub reject_reason: Option<&'static str>,
}

pub struct ProfitGate {
    pub min_profit_bps: u32,
    pub min_profit_usd: f64,
    pub safety_margin_bps: u32,
    pub stable_pool_extra_margin_bps: u32,
    pub token_usd_prices: HashMap<Address, f64>,
}

impl ProfitGate {
    pub fn new(
        min_profit_bps: u32,
        min_profit_usd: f64,
        safety_margin_bps: u32,
        stable_pool_extra_margin_bps: u32,
        token_usd_prices: HashMap<Address, f64>,
    ) -> Self {
        Self {
            min_profit_bps,
            min_profit_usd,
            safety_margin_bps,
            stable_pool_extra_margin_bps,
            token_usd_prices,
        }
    }

    pub fn should_submit(&self, result: &SimResult, path: &PathTemplate) -> Decision {
        if result.profit_bps < self.min_profit_bps {
            return Decision {
                accept: false,
                effective_profit_usd: 0.0,
                reject_reason: Some("below_min_bps"),
            };
        }

        // Extra margin for protocols with approximate off-chain math.
        // PancakeStable (Curve-style) has complex Newton iteration that may differ from on-chain.
        // AerodromeV2 volatile pools are wei-exact (validated). Aerodrome stable pools are also
        // wei-exact after the _f/_d/_get_y fix. We keep extra margin only for PancakeStable.
        let has_stable = path.hops.iter().any(|h| {
            matches!(h.protocol, Protocol::PancakeStable)
        });

        let total_margin = self.safety_margin_bps
            + if has_stable {
                self.stable_pool_extra_margin_bps
            } else {
                0
            };

        if result.profit_bps <= total_margin {
            return Decision {
                accept: false,
                effective_profit_usd: 0.0,
                reject_reason: Some("below_safety_margin"),
            };
        }

        let effective_bps = result.profit_bps - total_margin;

        // Convert gross profit to USD
        let token_price = self
            .token_usd_prices
            .get(&result.flash_token)
            .copied()
            .unwrap_or(1.0);

        // Determine token decimals from the flash amount magnitude
        // Heuristic: if flash_amount > 1e15, it's an 18-decimal token
        let decimals_factor = if result.flash_amount > U256::from(1_000_000_000_000_000u64) {
            1e18
        } else {
            1e6
        };

        let gross_profit_f64: f64 = result
            .gross_profit
            .try_into()
            .map(|v: u128| v as f64 / decimals_factor)
            .unwrap_or(f64::MAX);

        let profit_usd = gross_profit_f64 * token_price;
        let effective_profit_usd =
            profit_usd * (effective_bps as f64 / result.profit_bps as f64);

        if effective_profit_usd < self.min_profit_usd {
            return Decision {
                accept: false,
                effective_profit_usd,
                reject_reason: Some("below_min_usd"),
            };
        }

        Decision {
            accept: true,
            effective_profit_usd,
            reject_reason: None,
        }
    }
}
