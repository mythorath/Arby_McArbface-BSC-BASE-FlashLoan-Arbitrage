use std::collections::HashMap;

use alloy_primitives::{Address, U256};
use tracing::{debug, info};

use arb_core::types::PoolState;
use arb_state::pool_store::PoolStore;

const Q96: U256 = U256::from_limbs([0, 0x1_0000_0000, 0, 0]);

/// Derives USD prices for non-major tokens by walking through known pools
/// to find one that pairs them with a token of known USD price.
/// `token_decimals` maps token addresses to their ERC-20 decimals for
/// proper reserve normalization across different-decimal pairs.
pub fn derive_prices(
    store: &PoolStore,
    known_prices: &mut HashMap<Address, f64>,
    token_decimals: &HashMap<Address, u32>,
) -> usize {
    let mut derived = 0usize;
    let mut changed = true;

    while changed {
        changed = false;
        let snapshot: Vec<(Address, PoolState)> = store.get_all();

        for (_, state) in &snapshot {
            let (t0, t1, raw_ratio) = match state {
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

            let dec0 = token_decimals.get(&t0).copied().unwrap_or(18) as i32;
            let dec1 = token_decimals.get(&t1).copied().unwrap_or(18) as i32;
            let price_ratio = raw_ratio * 10f64.powi(dec0 - dec1);

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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use arb_core::types::{V2PoolState, V3PoolState};

    const WBNB: Address = address!("bb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c");
    const TOKEN_X: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const TOKEN_Y: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    #[test]
    fn test_derive_no_pools() {
        let store = PoolStore::new();
        let mut prices = HashMap::new();
        prices.insert(WBNB, 300.0);
        let decimals = HashMap::new();
        let derived = derive_prices(&store, &mut prices, &decimals);
        assert_eq!(derived, 0);
        assert_eq!(prices.len(), 1);
    }

    #[test]
    fn test_derive_v2_price() {
        let store = PoolStore::new();
        let pool_addr = address!("1111111111111111111111111111111111111111");
        store.update(pool_addr, PoolState::V2(V2PoolState {
            address: pool_addr,
            token0: WBNB,
            token1: TOKEN_X,
            reserve0: U256::from(1_000_000_000_000_000_000u64),
            reserve1: U256::from(2_000_000_000_000_000_000u64),
            fee_bps: 30,
        }));

        let mut prices = HashMap::new();
        prices.insert(WBNB, 300.0);

        let decimals = HashMap::new();
        let derived = derive_prices(&store, &mut prices, &decimals);
        assert_eq!(derived, 1);
        let token_x_price = prices.get(&TOKEN_X).unwrap();
        assert!((*token_x_price - 150.0).abs() < 0.01);
    }

    #[test]
    fn test_derive_cross_decimal_price() {
        let store = PoolStore::new();
        let usdc = address!("cccccccccccccccccccccccccccccccccccccccc");
        let pool_addr = address!("1111111111111111111111111111111111111111");
        store.update(pool_addr, PoolState::V2(V2PoolState {
            address: pool_addr,
            token0: WBNB,
            token1: usdc,
            reserve0: U256::from(1_000_000_000_000_000_000u64), // 1 WBNB (18 dec)
            reserve1: U256::from(300_000_000u64),                // 300 USDC (6 dec)
            fee_bps: 30,
        }));

        let mut prices = HashMap::new();
        prices.insert(WBNB, 300.0);

        let mut decimals = HashMap::new();
        decimals.insert(WBNB, 18u32);
        decimals.insert(usdc, 6u32);

        let derived = derive_prices(&store, &mut prices, &decimals);
        assert_eq!(derived, 1);
        let usdc_price = prices.get(&usdc).unwrap();
        assert!((*usdc_price - 1.0).abs() < 0.01, "USDC should be ~$1, got {usdc_price}");
    }

    #[test]
    fn test_derive_chain_propagation() {
        let store = PoolStore::new();

        let pool_ab = address!("1111111111111111111111111111111111111111");
        store.update(pool_ab, PoolState::V2(V2PoolState {
            address: pool_ab,
            token0: WBNB,
            token1: TOKEN_X,
            reserve0: U256::from(1_000_000_000_000_000_000u64),
            reserve1: U256::from(1_000_000_000_000_000_000u64),
            fee_bps: 30,
        }));

        let pool_bc = address!("2222222222222222222222222222222222222222");
        store.update(pool_bc, PoolState::V2(V2PoolState {
            address: pool_bc,
            token0: TOKEN_X,
            token1: TOKEN_Y,
            reserve0: U256::from(1_000_000_000_000_000_000u64),
            reserve1: U256::from(4_000_000_000_000_000_000u64),
            fee_bps: 30,
        }));

        let mut prices = HashMap::new();
        prices.insert(WBNB, 300.0);

        let decimals = HashMap::new();
        let derived = derive_prices(&store, &mut prices, &decimals);
        assert_eq!(derived, 2);
        assert!(prices.contains_key(&TOKEN_X));
        assert!(prices.contains_key(&TOKEN_Y));
        let y_price = prices.get(&TOKEN_Y).unwrap();
        assert!((*y_price - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_no_derive_both_unknown() {
        let store = PoolStore::new();
        let pool_addr = address!("1111111111111111111111111111111111111111");
        store.update(pool_addr, PoolState::V2(V2PoolState {
            address: pool_addr,
            token0: TOKEN_X,
            token1: TOKEN_Y,
            reserve0: U256::from(1_000_000_000_000_000_000u64),
            reserve1: U256::from(2_000_000_000_000_000_000u64),
            fee_bps: 30,
        }));

        let mut prices = HashMap::new();
        let decimals = HashMap::new();
        let derived = derive_prices(&store, &mut prices, &decimals);
        assert_eq!(derived, 0);
        assert!(!prices.contains_key(&TOKEN_X));
        assert!(!prices.contains_key(&TOKEN_Y));
    }
}
