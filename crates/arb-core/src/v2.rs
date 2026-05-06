use alloy_primitives::{Address, U256};

use crate::types::V2PoolState;
use crate::{AmmQuoter, QuoteError};

impl AmmQuoter for V2PoolState {
    fn quote(&self, token_in: Address, amount_in: U256) -> Result<U256, QuoteError> {
        if amount_in.is_zero() {
            return Err(QuoteError::ZeroInput);
        }

        let (reserve_in, reserve_out) = if token_in == self.token0 {
            (self.reserve0, self.reserve1)
        } else {
            (self.reserve1, self.reserve0)
        };

        if reserve_in.is_zero() || reserve_out.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: self.address,
            });
        }

        get_amount_out(amount_in, reserve_in, reserve_out, self.fee_bps)
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

/// Constant-product AMM: amountOut = (amountIn * fee_factor * reserveOut) / (reserveIn * 10000 + amountIn * fee_factor)
/// fee_bps: e.g. 25 for PancakeSwap V2 (0.25%), 10 for BiSwap, 30 for Uniswap V2
pub fn get_amount_out(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> Result<U256, QuoteError> {
    let fee_factor = U256::from(10000 - fee_bps);
    let amount_in_with_fee = amount_in
        .checked_mul(fee_factor)
        .ok_or(QuoteError::Overflow {
            context: "v2 amount_in * fee_factor",
        })?;

    let numerator = amount_in_with_fee
        .checked_mul(reserve_out)
        .ok_or(QuoteError::Overflow {
            context: "v2 numerator",
        })?;

    let denominator = reserve_in
        .checked_mul(U256::from(10000u32))
        .ok_or(QuoteError::Overflow {
            context: "v2 denominator base",
        })?
        .checked_add(amount_in_with_fee)
        .ok_or(QuoteError::Overflow {
            context: "v2 denominator sum",
        })?;

    Ok(numerator / denominator)
}

/// Given a desired output, compute the required input (for reverse-quoting / path optimization)
pub fn get_amount_in(
    amount_out: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> Result<U256, QuoteError> {
    if amount_out >= reserve_out {
        return Err(QuoteError::ZeroLiquidity {
            pool: Address::ZERO,
        });
    }
    let fee_factor = U256::from(10000 - fee_bps);
    let numerator = reserve_in
        .checked_mul(amount_out)
        .ok_or(QuoteError::Overflow {
            context: "v2 getAmountIn numerator",
        })?
        .checked_mul(U256::from(10000u32))
        .ok_or(QuoteError::Overflow {
            context: "v2 getAmountIn numerator*10000",
        })?;
    let denominator = (reserve_out - amount_out)
        .checked_mul(fee_factor)
        .ok_or(QuoteError::Overflow {
            context: "v2 getAmountIn denominator",
        })?;
    Ok(numerator / denominator + U256::from(1u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_swap() {
        let amount_out = get_amount_out(
            U256::from(1_000_000u64),
            U256::from(1_000_000_000u64),
            U256::from(1_000_000_000u64),
            25, // PCS V2 fee
        )
        .unwrap();
        assert!(amount_out > U256::ZERO);
        assert!(amount_out < U256::from(1_000_000u64));
    }

    #[test]
    fn test_zero_input() {
        let result = get_amount_out(
            U256::ZERO,
            U256::from(1_000_000u64),
            U256::from(1_000_000u64),
            25,
        );
        assert_eq!(result.unwrap(), U256::ZERO);
    }

    #[test]
    fn test_round_trip() {
        let reserve = U256::from(1_000_000_000_000u64);
        let amount_in = U256::from(1_000_000u64);
        let fee = 30u32;

        let out = get_amount_out(amount_in, reserve, reserve, fee).unwrap();
        let back_in = get_amount_in(out, reserve, reserve, fee).unwrap();
        assert!(back_in >= amount_in);
    }
}
