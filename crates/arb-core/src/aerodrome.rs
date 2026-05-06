use alloy_primitives::{Address, U256};

use crate::types::AeroV2PoolState;
use crate::{AmmQuoter, QuoteError};

const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);

impl AmmQuoter for AeroV2PoolState {
    fn quote(&self, token_in: Address, amount_in: U256) -> Result<U256, QuoteError> {
        if amount_in.is_zero() {
            return Err(QuoteError::ZeroInput);
        }
        if self.reserve0.is_zero() || self.reserve1.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: self.address,
            });
        }

        let fee_amount = amount_in * U256::from(self.fee_bps) / U256::from(10000u32);
        let amount_in_after_fee = amount_in - fee_amount;

        if self.stable {
            get_amount_out_stable(
                amount_in_after_fee,
                token_in,
                self.token0,
                self.reserve0,
                self.reserve1,
                self.decimals0,
                self.decimals1,
            )
        } else {
            quote_volatile(
                amount_in_after_fee,
                token_in,
                self.token0,
                self.reserve0,
                self.reserve1,
            )
        }
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

fn quote_volatile(
    amount_in: U256,
    token_in: Address,
    token0: Address,
    reserve0: U256,
    reserve1: U256,
) -> Result<U256, QuoteError> {
    let (reserve_in, reserve_out) = if token_in == token0 {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };

    let numerator = amount_in
        .checked_mul(reserve_out)
        .ok_or(QuoteError::Overflow {
            context: "aero volatile numerator",
        })?;
    let denominator = reserve_in
        .checked_add(amount_in)
        .ok_or(QuoteError::Overflow {
            context: "aero volatile denominator",
        })?;

    Ok(numerator / denominator)
}

/// Exact port of Aerodrome Pool.sol `_f(x0, y)`.
/// Operates on already-scaled (1e18-normalised) values.
///   _f(x0, y) = (x0 * y / 1e18) * (x0^2/1e18 + y^2/1e18) / 1e18
fn f(x0: U256, y: U256) -> U256 {
    let a = (x0 * y) / WAD;
    let b = (x0 * x0) / WAD + (y * y) / WAD;
    (a * b) / WAD
}

/// Exact port of Aerodrome Pool.sol `_d(x0, y)`.
/// Derivative of _f with respect to y.
///   _d(x0, y) = 3*x0*(y^2/1e18)/1e18  +  (x0^2/1e18)*x0/1e18
fn d(x0: U256, y: U256) -> U256 {
    (U256::from(3) * x0 * ((y * y) / WAD)) / WAD + ((((x0 * x0) / WAD) * x0) / WAD)
}

/// Exact port of Aerodrome Pool.sol `_k(x, y)`.
/// Computes the invariant with decimal scaling.
fn k(x: U256, y: U256, decimals0: U256, decimals1: U256) -> U256 {
    let _x = (x * WAD) / decimals0;
    let _y = (y * WAD) / decimals1;
    let a = (_x * _y) / WAD;
    let b = ((_x * _x) / WAD) + ((_y * _y) / WAD);
    (a * b) / WAD
}

/// Exact port of Aerodrome Pool.sol `_get_y(x0, xy, y)`.
/// Newton iteration to find y such that f(x0, y) = xy.
/// All inputs are already scaled to 1e18.
fn get_y(x0: U256, xy: U256, mut y: U256) -> Result<U256, QuoteError> {
    for _ in 0..255 {
        let k_val = f(x0, y);
        if k_val < xy {
            let dy = ((xy - k_val) * WAD) / d(x0, y);
            if dy.is_zero() {
                if k_val == xy {
                    return Ok(y);
                }
                if f(x0, y + U256::from(1)) > xy {
                    return Ok(y + U256::from(1));
                }
                y = y + U256::from(1);
            } else {
                y = y + dy;
            }
        } else {
            let dy = ((k_val - xy) * WAD) / d(x0, y);
            if dy.is_zero() {
                if k_val == xy || f(x0, y - U256::from(1)) < xy {
                    return Ok(y);
                }
                y = y - U256::from(1);
            } else {
                y = y - dy;
            }
        }
    }
    Err(QuoteError::Overflow {
        context: "aero stable get_y did not converge",
    })
}

/// Exact port of Aerodrome Pool.sol `_getAmountOut` for stable pools.
/// Fee has already been deducted from amount_in before calling.
fn get_amount_out_stable(
    amount_in: U256,
    token_in: Address,
    token0: Address,
    reserve0: U256,
    reserve1: U256,
    decimals0: U256,
    decimals1: U256,
) -> Result<U256, QuoteError> {
    let xy = k(reserve0, reserve1, decimals0, decimals1);

    let scaled_r0 = (reserve0 * WAD) / decimals0;
    let scaled_r1 = (reserve1 * WAD) / decimals1;

    let (reserve_a, reserve_b, dec_in, dec_out) = if token_in == token0 {
        (scaled_r0, scaled_r1, decimals0, decimals1)
    } else {
        (scaled_r1, scaled_r0, decimals1, decimals0)
    };

    let scaled_amount_in = (amount_in * WAD) / dec_in;
    let y = reserve_b - get_y(scaled_amount_in + reserve_a, xy, reserve_b)?;
    Ok((y * dec_out) / WAD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volatile_basic() {
        let pool = AeroV2PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            reserve0: U256::from(1_000_000_000_000u64),
            reserve1: U256::from(1_000_000_000_000u64),
            stable: false,
            fee_bps: 30,
            decimals0: U256::from(1_000_000_000_000_000_000u64),
            decimals1: U256::from(1_000_000_000_000_000_000u64),
        };
        let out = pool
            .quote(pool.token0, U256::from(1_000_000u64))
            .unwrap();
        assert!(out > U256::ZERO);
        assert!(out < U256::from(1_000_000u64));
    }

    #[test]
    fn test_stable_same_decimals() {
        // Two 18-decimal tokens, equal reserves
        let pool = AeroV2PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            reserve0: U256::from(1_000_000u64) * U256::from(1_000_000_000_000_000_000u64),
            reserve1: U256::from(1_000_000u64) * U256::from(1_000_000_000_000_000_000u64),
            stable: true,
            fee_bps: 2,
            decimals0: U256::from(1_000_000_000_000_000_000u64), // 1e18
            decimals1: U256::from(1_000_000_000_000_000_000u64), // 1e18
        };
        let swap_amount = U256::from(1_000u64) * U256::from(1_000_000_000_000_000_000u64);
        let out = pool.quote(pool.token0, swap_amount).unwrap();
        assert!(out > U256::ZERO);
        // Stable pool with equal reserves: output should be close to input (minus fee)
        assert!(out < swap_amount);
        // Should be very close for a stable curve with large reserves
        let min_expected = swap_amount * U256::from(99u32) / U256::from(100u32);
        assert!(out > min_expected, "out={out}, min_expected={min_expected}");
    }

    #[test]
    fn test_stable_usdc_weth_different_decimals() {
        // USDC (6 decimals) / WETH (18 decimals) — but treated as same-priced for stable
        // This tests that decimal scaling works correctly
        let decimals0 = U256::from(1_000_000u64);                // 10^6  (USDC)
        let decimals1 = U256::from(1_000_000_000_000_000_000u64); // 10^18 (WETH-like)

        let pool = AeroV2PoolState {
            address: Address::ZERO,
            token0: Address::with_last_byte(1),
            token1: Address::with_last_byte(2),
            reserve0: U256::from(10_000_000_000u64),   // 10,000 USDC (6 dec)
            reserve1: U256::from(10_000u64) * U256::from(1_000_000_000_000_000_000u64), // 10,000 tokens (18 dec)
            stable: true,
            fee_bps: 2,
            decimals0,
            decimals1,
        };

        // Swap 1 USDC
        let amount_in = U256::from(1_000_000u64); // 1 USDC
        let out = pool.quote(pool.token0, amount_in).unwrap();
        assert!(out > U256::ZERO);
        // Output should be roughly 1 token (in 18-decimal), minus tiny fee and slippage
        let one_token_18 = U256::from(1_000_000_000_000_000_000u64);
        assert!(out < one_token_18, "out should be less than 1 full token");
        // Should be close to 1e18 (within 1%)
        let min = one_token_18 * U256::from(98u32) / U256::from(100u32);
        assert!(out > min, "out={out}, min={min}");
    }
}
