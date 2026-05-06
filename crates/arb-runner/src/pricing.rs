use std::collections::HashMap;

use alloy_primitives::{Address, U256};
use tracing::{debug, info};

use arb_core::types::PoolState;
use arb_state::pool_store::PoolStore;

const Q96: U256 = U256::from_limbs([0, 0x1_0000_0000, 0, 0]);

/// Derives USD prices for non-major tokens by walking through known pools
/// to find one that pairs them with a token of known USD price.
pub fn derive_prices(
    store: &PoolStore,
    known_prices: &mut HashMap<Address, f64>,
) -> usize {
    let mut derived = 0usize;
    let mut changed = true;

    while changed {
        changed = false;
        let snapshot: Vec<(Address, PoolState)> = store.get_all();

        for (_, state) in &snapshot {
            let (t0, t1, price_ratio) = match state {
                PoolState::V2(s) => {
                    if s.reserve0.is_zero() || s.reserve1.is_zero() {
                        continue;
                    }
                    let r0: f64 = s.reserve0.try_into()
                        .map(|v: u128| v as f64).unwrap_or(0.0);
                    let r1: f64 = s.reserve1.try_into()
                        .map(|v: u128| v as f64).unwrap_or(0.0);
                    if r0 == 0.0 || r1 == 0.0 { continue; }
                    (s.token0, s.token1, r1 / r0)
                }
                PoolState::V3(s) => {
                    if s.sqrt_price_x96.is_zero() || s.liquidity == 0 {
                        continue;
                    }
                    let sqrt_p: f64 = s.sqrt_price_x96.try_into()
                        .map(|v: u128| v as f64).unwrap_or(0.0);
                    let q96_f: f64 = Q96.try_into()
                        .map(|v: u128| v as f64).unwrap_or(1.0);
                    let price = (sqrt_p / q96_f).powi(2);
                    (s.token0, s.token1, price)
                }
                PoolState::AeroV2(s) => {
                    if s.reserve0.is_zero() || s.reserve1.is_zero() {
                        continue;
                    }
                    let r0: f64 = s.reserve0.try_into()
                        .map(|v: u128| v as f64).unwrap_or(0.0);
                    let r1: f64 = s.reserve1.try_into()
                        .map(|v: u128| v as f64).unwrap_or(0.0);
                    if r0 == 0.0 || r1 == 0.0 { continue; }
                    (s.token0, s.token1, r1 / r0)
                }
                _ => continue,
            };

            if price_ratio <= 0.0 || price_ratio.is_nan() || price_ratio.is_infinite() {
                continue;
            }

            let known_t0 = known_prices.get(&t0).copied();
            let known_t1 = known_prices.get(&t1).copied();

            match (known_t0, known_t1) {
                (Some(p0), None) => {
                    let p1 = p0 / price_ratio;
                    if p1 > 0.0 && p1 < 1e12 {
                        known_prices.insert(t1, p1);
                        derived += 1;
                        changed = true;
                        debug!(token = %t1, price = p1, via = %t0, "Derived price");
                    }
                }
                (None, Some(p1)) => {
                    let p0 = p1 * price_ratio;
                    if p0 > 0.0 && p0 < 1e12 {
                        known_prices.insert(t0, p0);
                        derived += 1;
                        changed = true;
                        debug!(token = %t0, price = p0, via = %t1, "Derived price");
                    }
                }
                _ => {}
            }
        }
    }

    if derived > 0 {
        info!(derived, total = known_prices.len(), "Price derivation complete");
    }

    derived
}
