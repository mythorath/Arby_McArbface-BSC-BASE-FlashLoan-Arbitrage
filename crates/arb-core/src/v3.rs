use alloy_primitives::{Address, U256};

use crate::types::V3PoolState;
use crate::{AmmQuoter, QuoteError};

const Q96: U256 = U256::from_limbs([0, 0x1_0000_0000, 0, 0]); // 2^96

impl AmmQuoter for V3PoolState {
    fn quote(&self, token_in: Address, amount_in: U256) -> Result<U256, QuoteError> {
        if amount_in.is_zero() {
            return Err(QuoteError::ZeroInput);
        }
        if self.liquidity == 0 {
            return Err(QuoteError::ZeroLiquidity {
                pool: self.address,
            });
        }
        if self.sqrt_price_x96.is_zero() {
            return Err(QuoteError::NotInitialized);
        }

        let zero_for_one = token_in == self.token0;

        let fee = if zero_for_one {
            self.fee
        } else {
            self.fee_otz.unwrap_or(self.fee)
        };

        quote_single_tick(
            amount_in,
            self.sqrt_price_x96,
            self.liquidity,
            fee,
            zero_for_one,
        )
    }

    fn pool_id(&self) -> Address {
        self.address
    }
    fn token0(&self) -> Address {
        self.token0
    }
    fn token1(&self) -> Address {
        self.token1
    }
}

/// Quote a swap within a single tick range (no tick crossing).
///
/// Uses the V3 math: for exact-input zeroForOne:
///   sqrtPrice_next = L * sqrtPrice / (L + amountIn * sqrtPrice)
///   amountOut = L * (sqrtPrice - sqrtPrice_next)
///
/// For exact-input oneForZero:
///   sqrtPrice_next = sqrtPrice + amountIn / L
///   amountOut = L * (1/sqrtPrice_next - 1/sqrtPrice) [via Q notation]
fn quote_single_tick(
    amount_in: U256,
    sqrt_price_x96: U256,
    liquidity: u128,
    fee: u32,
    zero_for_one: bool,
) -> Result<U256, QuoteError> {
    let fee_amount = amount_in * U256::from(fee) / U256::from(1_000_000u32);
    let amount_in_after_fee = amount_in - fee_amount;
    let l = U256::from(liquidity);

    if zero_for_one {
        // token0 -> token1
        // new_sqrt_price = L * sqrt_price / (L + amount_in * sqrt_price / Q96)
        let product = amount_in_after_fee
            .checked_mul(sqrt_price_x96)
            .ok_or(QuoteError::Overflow {
                context: "v3 zfo product",
            })?;
        let scaled_amount = product / Q96;
        let denominator = l
            .checked_add(scaled_amount)
            .ok_or(QuoteError::Overflow {
                context: "v3 zfo denominator",
            })?;

        if denominator.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: Address::ZERO,
            });
        }

        let new_sqrt_price = l
            .checked_mul(sqrt_price_x96)
            .ok_or(QuoteError::Overflow {
                context: "v3 zfo new_sqrt numerator",
            })?
            / denominator;

        // amount1_out = L * (sqrt_price - new_sqrt_price) / Q96
        if sqrt_price_x96 <= new_sqrt_price {
            return Ok(U256::ZERO);
        }
        let delta = sqrt_price_x96 - new_sqrt_price;
        let amount_out = l
            .checked_mul(delta)
            .ok_or(QuoteError::Overflow {
                context: "v3 zfo amount_out",
            })?
            / Q96;

        Ok(amount_out)
    } else {
        // token1 -> token0
        // new_sqrt_price = sqrt_price + amount_in * Q96 / L
        let scaled = amount_in_after_fee
            .checked_mul(Q96)
            .ok_or(QuoteError::Overflow {
                context: "v3 ofz scaled",
            })?;

        if l.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: Address::ZERO,
            });
        }

        let delta_sqrt = scaled / l;
        let new_sqrt_price = sqrt_price_x96
            .checked_add(delta_sqrt)
            .ok_or(QuoteError::Overflow {
                context: "v3 ofz new_sqrt",
            })?;

        // amount0_out = L * Q96 * (new - old) / (old * new)
        // Equivalently: L * (1/old - 1/new) in Q96 terms
        if new_sqrt_price.is_zero() || sqrt_price_x96.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: Address::ZERO,
            });
        }

        // amount0 = L * (new_sqrt - old_sqrt) * Q96 / (old_sqrt * new_sqrt)  — but we want the DECREASE in token0
        // Actually for oneForZero, price goes UP, so token0 comes out:
        // amount0_out = L * Q96 * (1/sqrt_price_old - 1/sqrt_price_new)
        //             = L * Q96 * (new_sqrt - old_sqrt) / (old_sqrt * new_sqrt)   ... wait that's wrong sign
        // Let's use: amount0 = L * (Q96/old - Q96/new) = L * Q96 * (new - old) / (old * new)
        let price_diff = new_sqrt_price - sqrt_price_x96;
        let denom = sqrt_price_x96
            .checked_mul(new_sqrt_price)
            .ok_or(QuoteError::Overflow {
                context: "v3 ofz price product",
            })?;

        let amount_out = l
            .checked_mul(Q96)
            .ok_or(QuoteError::Overflow {
                context: "v3 ofz L*Q96",
            })?
            .checked_mul(price_diff)
            .ok_or(QuoteError::Overflow {
                context: "v3 ofz L*Q96*diff",
            })?
            / denom;

        Ok(amount_out)
    }
}

/// Multi-tick approximation for thin V3 pools.
/// Instead of true tick walking (which requires bitmap state we don't store),
/// this uses a piecewise approach: if the single-tick quote would consume more
/// than a threshold fraction of the available liquidity, we haircut the output
/// to model increased slippage from tick crossings.
///
/// The haircut is conservative: better to underestimate output (miss a marginal
/// arb) than overestimate (submit a reverting tx).
pub fn quote_multi_tick_approx(
    amount_in: U256,
    sqrt_price_x96: U256,
    liquidity: u128,
    fee: u32,
    zero_for_one: bool,
) -> Result<U256, QuoteError> {
    let base = quote_single_tick(amount_in, sqrt_price_x96, liquidity, fee, zero_for_one)?;
    if base.is_zero() {
        return Ok(U256::ZERO);
    }

    let l = U256::from(liquidity);
    let q96 = U256::from(1u128 << 96);

    // Estimate in-range reserve on the input side
    let reserve_in = if zero_for_one {
        l.checked_mul(q96)
            .and_then(|v| v.checked_div(sqrt_price_x96))
            .unwrap_or(U256::MAX)
    } else {
        l.checked_mul(sqrt_price_x96)
            .and_then(|v| v.checked_div(q96))
            .unwrap_or(U256::MAX)
    };

    if reserve_in.is_zero() {
        return Ok(U256::ZERO);
    }

    // Compute utilization: what fraction of in-range reserve are we consuming?
    // If utilization is < 30%, single-tick is accurate enough.
    // Above 30%, apply a quadratic haircut to model tick-crossing slippage.
    let fee_amount = amount_in * U256::from(fee) / U256::from(1_000_000u32);
    let amount_after_fee = amount_in - fee_amount;
    let utilization_bps = (amount_after_fee * U256::from(10000u32)) / reserve_in;
    let util: u64 = utilization_bps.try_into().unwrap_or(10000);

    if util <= 3000 {
        return Ok(base);
    }

    // Quadratic haircut: output *= (1 - ((util - 0.3) / 0.7)^2 * 0.5)
    // At 100% utilization, haircut is 50% (very conservative)
    let excess = util.saturating_sub(3000); // 0..7000 range
    let penalty_bps = (excess as u128 * excess as u128 * 5000) / (7000 * 7000); // quadratic
    let multiplier = 10000u64.saturating_sub(penalty_bps as u64);

    Ok((base * U256::from(multiplier)) / U256::from(10000u32))
}

/// Convert a tick to sqrtPriceX96: sqrt(1.0001^tick) * 2^96
/// Used when we need to work with tick boundaries.
pub fn tick_to_sqrt_price_x96(tick: i32) -> U256 {
    // For the single-tick approximation we use in quote, we don't need this.
    // But it's useful for validation. We use a simplified float-based approach
    // that's accurate enough for path scoring (not for on-chain execution).
    let price = 1.0001_f64.powi(tick);
    let sqrt_price = price.sqrt();
    let q96_f64 = 2.0_f64.powi(96);
    let result = sqrt_price * q96_f64;
    U256::from(result as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pool(sqrt_price: U256, liq: u128, fee: u32) -> V3PoolState {
        V3PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            sqrt_price_x96: sqrt_price,
            tick: 0,
            liquidity: liq,
            fee,
            fee_otz: None,
        }
    }

    #[test]
    fn test_v3_basic_zero_for_one() {
        // ~1:1 price, high liquidity
        let pool = make_pool(Q96, 1_000_000_000_000u128, 3000);
        let out = pool
            .quote(pool.token0, U256::from(1_000_000u64))
            .unwrap();
        assert!(out > U256::ZERO);
        assert!(out < U256::from(1_000_000u64));
    }

    #[test]
    fn test_v3_basic_one_for_zero() {
        let pool = make_pool(Q96, 1_000_000_000_000u128, 3000);
        let out = pool
            .quote(pool.token1, U256::from(1_000_000u64))
            .unwrap();
        assert!(out > U256::ZERO);
        assert!(out < U256::from(1_000_000u64));
    }

    #[test]
    fn test_v3_zero_liquidity() {
        let pool = make_pool(Q96, 0, 3000);
        assert!(pool.quote(pool.token0, U256::from(1000u64)).is_err());
    }

    #[test]
    fn test_multi_tick_small_swap_no_haircut() {
        let liq = 1_000_000_000_000_000u128;
        let fee = 3000u32;
        let amount = U256::from(1_000u64);

        let single = quote_single_tick(amount, Q96, liq, fee, true).unwrap();
        let multi = quote_multi_tick_approx(amount, Q96, liq, fee, true).unwrap();

        assert_eq!(single, multi, "small swap should produce identical output");
    }

    #[test]
    fn test_multi_tick_large_swap_has_haircut() {
        let liq = 1_000_000u128;
        let fee = 3000u32;
        let amount = U256::from(500_000u64);

        let single = quote_single_tick(amount, Q96, liq, fee, true).unwrap();
        let multi = quote_multi_tick_approx(amount, Q96, liq, fee, true).unwrap();

        assert!(multi < single, "large swap should get haircut: multi={multi}, single={single}");
        assert!(multi > U256::ZERO, "output should still be positive");
    }

    #[test]
    fn test_multi_tick_zero_input() {
        let result = quote_multi_tick_approx(U256::ZERO, Q96, 1_000_000u128, 3000, true);
        assert_eq!(result.unwrap(), U256::ZERO);
    }

    #[test]
    fn test_multi_tick_zero_liquidity() {
        let result = quote_multi_tick_approx(U256::from(1000u64), Q96, 0, 3000, false);
        assert!(result.is_err(), "zero liquidity should return an error");
    }

    #[test]
    fn test_multi_tick_one_for_zero_small() {
        let liq = 1_000_000_000_000_000u128;
        let fee = 3000u32;
        let amount = U256::from(1_000u64);

        let single = quote_single_tick(amount, Q96, liq, fee, false).unwrap();
        let multi = quote_multi_tick_approx(amount, Q96, liq, fee, false).unwrap();

        assert_eq!(single, multi, "small one_for_zero swap should have no haircut");
    }

    #[test]
    fn test_multi_tick_one_for_zero_large() {
        let liq = 10_000u128;
        let fee = 3000u32;
        let amount = U256::from(100_000u64);

        let single = quote_single_tick(amount, Q96, liq, fee, false).unwrap();
        let multi = quote_multi_tick_approx(amount, Q96, liq, fee, false).unwrap();

        assert!(multi <= single, "large one_for_zero should get haircut or equal");
    }

    #[test]
    fn test_tick_to_sqrt_price() {
        let result = tick_to_sqrt_price_x96(0);
        let diff = if result > Q96 { result - Q96 } else { Q96 - result };
        assert!(
            diff <= U256::from(1u64),
            "tick 0 should give sqrtPriceX96 ≈ Q96 (2^96), got {result}"
        );
    }

    #[test]
    fn test_algebra_directional_fee_zto() {
        let pool = V3PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            sqrt_price_x96: Q96,
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee: 100,
            fee_otz: Some(500),
        };
        let out_zto = pool.quote(pool.token0, U256::from(1_000_000u64)).unwrap();
        let pool_same = V3PoolState { fee_otz: None, ..pool.clone() };
        let out_same = pool_same.quote(pool_same.token0, U256::from(1_000_000u64)).unwrap();
        assert_eq!(out_zto, out_same, "zto should use fee field");
    }

    #[test]
    fn test_algebra_directional_fee_otz() {
        let pool = V3PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            sqrt_price_x96: Q96,
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee: 100,
            fee_otz: Some(500),
        };
        let out_otz = pool.quote(pool.token1, U256::from(1_000_000u64)).unwrap();
        let pool_500 = V3PoolState { fee: 500, fee_otz: None, ..pool.clone() };
        let out_500 = pool_500.quote(pool_500.token1, U256::from(1_000_000u64)).unwrap();
        assert_eq!(out_otz, out_500, "otz should use fee_otz when set");
    }

    #[test]
    fn test_fee_otz_none_fallback() {
        let pool = V3PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            sqrt_price_x96: Q96,
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee: 3000,
            fee_otz: None,
        };
        let out_zto = pool.quote(pool.token0, U256::from(1_000u64)).unwrap();
        let out_otz = pool.quote(pool.token1, U256::from(1_000u64)).unwrap();
        assert_eq!(out_zto, out_otz);
    }
}
