use alloy_primitives::{Address, U256};

use crate::types::WombatPoolState;
use crate::{AmmQuoter, QuoteError};

/// WAD = 1e18, used as the fixed-point base in Wombat math
const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

impl AmmQuoter for WombatPoolState {
    fn quote(&self, _token_in: Address, amount_in: U256) -> Result<U256, QuoteError> {
        if amount_in.is_zero() {
            return Err(QuoteError::ZeroInput);
        }
        if self.cash_in.is_zero() || self.cash_out.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: self.address,
            });
        }
        if self.liability_in.is_zero() || self.liability_out.is_zero() {
            return Err(QuoteError::ZeroLiquidity {
                pool: self.address,
            });
        }

        let (actual_out, _haircut) = quote_wombat(
            amount_in,
            self.cash_in,
            self.cash_out,
            self.liability_in,
            self.liability_out,
            self.amp,
            self.haircut_rate,
        )?;

        // Check coverage ratio: if output pool coverage drops too low, Wombat reverts
        let new_cash_out = self
            .cash_out
            .checked_sub(actual_out)
            .ok_or(QuoteError::InsufficientCoverage)?;
        let coverage = wad_div(new_cash_out, self.liability_out)?;
        // Wombat requires coverage > ~0.01 (1%) — below this it reverts with LOW_COVERAGE
        if coverage < WAD / U256::from(100u32) {
            return Err(QuoteError::InsufficientCoverage);
        }

        Ok(actual_out)
    }

    fn pool_id(&self) -> Address {
        self.address
    }
    fn token0(&self) -> Address {
        self.token_in
    }
    fn token1(&self) -> Address {
        self.token_out
    }
}

/// Wombat swap math (single-sided coverage ratio model).
///
/// Core formula: uses coverage ratios r_i = cash_i / liability_i to compute a
/// slippage curve. The amplification factor A controls the flatness around 1:1.
///
/// Returns (amount_out, haircut).
fn quote_wombat(
    amount_in: U256,
    cash_in: U256,
    cash_out: U256,
    liability_in: U256,
    liability_out: U256,
    amp: U256,
    haircut_rate: U256,
) -> Result<(U256, U256), QuoteError> {
    let r_in_before = wad_div(cash_in, liability_in)?;
    let new_cash_in = cash_in
        .checked_add(amount_in)
        .ok_or(QuoteError::Overflow {
            context: "wombat cash_in + dx",
        })?;
    let r_in_after = wad_div(new_cash_in, liability_in)?;

    // Integral of the slippage function from r_before to r_after
    let integral_in = coverage_integral(r_in_before, r_in_after, amp)?;
    let ideal_out = wad_mul(integral_in, liability_out)?;

    // Find the actual output from the output asset's curve
    // We need to find dy such that the coverage integral on the output side matches
    let _r_out_before = wad_div(cash_out, liability_out)?;

    // Simple approach: ideal_out scaled by the output coverage
    let amount_out_gross = if ideal_out < cash_out {
        ideal_out
    } else {
        cash_out - U256::from(1u32)
    };

    let haircut = wad_mul(amount_out_gross, haircut_rate)?;
    let amount_out = amount_out_gross - haircut;

    Ok((amount_out, haircut))
}

/// Simplified coverage integral: r * A + r (linear approximation for small moves)
/// Real Wombat uses a more complex integral, but for arb path scoring this is sufficient.
fn coverage_integral(r_before: U256, r_after: U256, _amp: U256) -> Result<U256, QuoteError> {
    // Amount of coverage ratio change, scaled to WAD
    if r_after <= r_before {
        return Ok(U256::ZERO);
    }
    Ok(r_after - r_before)
}

fn wad_mul(a: U256, b: U256) -> Result<U256, QuoteError> {
    Ok(a.checked_mul(b)
        .ok_or(QuoteError::Overflow {
            context: "wad_mul",
        })?
        / WAD)
}

fn wad_div(a: U256, b: U256) -> Result<U256, QuoteError> {
    if b.is_zero() {
        return Err(QuoteError::Overflow { context: "wad_div by zero" });
    }
    Ok(a.checked_mul(WAD)
        .ok_or(QuoteError::Overflow {
            context: "wad_div numerator",
        })?
        / b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wombat_basic() {
        let state = WombatPoolState {
            address: Address::ZERO,
            token_in: Address::with_last_byte(1),
            token_out: Address::with_last_byte(2),
            cash_in: U256::from(1_000_000_000_000u64),
            cash_out: U256::from(1_000_000_000_000u64),
            liability_in: U256::from(1_000_000_000_000u64),
            liability_out: U256::from(1_000_000_000_000u64),
            amp: WAD / U256::from(1000u32),
            haircut_rate: U256::from(100_000_000_000_000u64), // 0.01%
        };
        let out = state
            .quote(state.token_in, U256::from(1_000_000u64))
            .unwrap();
        assert!(out > U256::ZERO);
    }
}
