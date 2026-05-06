use alloy_primitives::{Address, U256};

use crate::types::DodoPoolState;
use crate::{AmmQuoter, QuoteError};

const ONE: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]); // 1e18

/// DODO V2 R-state
const R_ONE: u8 = 0;
const R_ABOVE_ONE: u8 = 1;
const R_BELOW_ONE: u8 = 2;

impl AmmQuoter for DodoPoolState {
    fn quote(&self, token_in: Address, amount_in: U256) -> Result<U256, QuoteError> {
        if amount_in.is_zero() {
            return Err(QuoteError::ZeroInput);
        }

        let is_sell_base = token_in == self.base_token;

        let (receive_amount, _) = if is_sell_base {
            sell_base_token(
                amount_in,
                self.base_reserve,
                self.quote_reserve,
                self.base_target,
                self.quote_target,
                self.r_state,
                self.k,
                self.lp_fee_rate,
                self.mt_fee_rate,
            )?
        } else {
            sell_quote_token(
                amount_in,
                self.base_reserve,
                self.quote_reserve,
                self.base_target,
                self.quote_target,
                self.r_state,
                self.k,
                self.lp_fee_rate,
                self.mt_fee_rate,
            )?
        };

        Ok(receive_amount)
    }

    fn pool_id(&self) -> Address {
        self.address
    }
    fn token0(&self) -> Address {
        self.base_token
    }
    fn token1(&self) -> Address {
        self.quote_token
    }
}

/// DODO PMM core: sell base tokens for quote tokens.
/// Returns (receiveQuoteAmount, totalFee).
fn sell_base_token(
    amount: U256,
    base_reserve: U256,
    _quote_reserve: U256,
    base_target: U256,
    _quote_target: U256,
    r_state: u8,
    k: U256,
    lp_fee_rate: U256,
    mt_fee_rate: U256,
) -> Result<(U256, U256), QuoteError> {
    let receive_quote = match r_state {
        R_ONE => {
            // equilibrium: use standard pricing
            general_integrate(base_target, base_reserve, base_reserve + amount, k)?
        }
        R_ABOVE_ONE => {
            // base surplus: straightforward
            general_integrate(base_target, base_reserve, base_reserve + amount, k)?
        }
        R_BELOW_ONE => {
            // base deficit: more complex, may cross equilibrium
            let back_to_one = base_target
                .checked_sub(base_reserve)
                .unwrap_or(U256::ZERO);
            if amount < back_to_one {
                general_integrate(base_target, base_reserve, base_reserve + amount, k)?
            } else {
                let part1 = general_integrate(base_target, base_reserve, base_target, k)?;
                let part2 = general_integrate(base_target, base_target, base_reserve + amount, k)?;
                part1 + part2
            }
        }
        _ => return Err(QuoteError::NotInitialized),
    };

    let lp_fee = receive_quote * lp_fee_rate / ONE;
    let mt_fee = receive_quote * mt_fee_rate / ONE;
    let total_fee = lp_fee + mt_fee;
    let actual = receive_quote.saturating_sub(total_fee);

    Ok((actual, total_fee))
}

/// DODO PMM core: sell quote tokens for base tokens.
fn sell_quote_token(
    amount: U256,
    _base_reserve: U256,
    quote_reserve: U256,
    _base_target: U256,
    quote_target: U256,
    r_state: u8,
    k: U256,
    lp_fee_rate: U256,
    mt_fee_rate: U256,
) -> Result<(U256, U256), QuoteError> {
    let receive_base = match r_state {
        R_ONE => general_integrate(quote_target, quote_reserve, quote_reserve + amount, k)?,
        R_BELOW_ONE => general_integrate(quote_target, quote_reserve, quote_reserve + amount, k)?,
        R_ABOVE_ONE => {
            let back_to_one = quote_target.checked_sub(quote_reserve).unwrap_or(U256::ZERO);
            if amount < back_to_one {
                general_integrate(quote_target, quote_reserve, quote_reserve + amount, k)?
            } else {
                let part1 = general_integrate(quote_target, quote_reserve, quote_target, k)?;
                let part2 =
                    general_integrate(quote_target, quote_target, quote_reserve + amount, k)?;
                part1 + part2
            }
        }
        _ => return Err(QuoteError::NotInitialized),
    };

    let lp_fee = receive_base * lp_fee_rate / ONE;
    let mt_fee = receive_base * mt_fee_rate / ONE;
    let total_fee = lp_fee + mt_fee;
    let actual = receive_base.saturating_sub(total_fee);

    Ok((actual, total_fee))
}

/// PMM integrate: ∫ from B1 to B2 of the pricing curve
/// result = target * (1 - k + k * target / B1) - target * (1 - k + k * target / B2)
///        ≈ target * k * target * (1/B1 - 1/B2) + target * (1-k) * (1 - 1) ... simplified:
/// = (B2 - B1) * target * (1 - k + k * target^2 / (B1 * B2)) ... approximately
fn general_integrate(
    target: U256,
    reserve_start: U256,
    reserve_end: U256,
    k: U256,
) -> Result<U256, QuoteError> {
    if reserve_start.is_zero() || reserve_end.is_zero() {
        return Err(QuoteError::ZeroLiquidity {
            pool: Address::ZERO,
        });
    }
    if reserve_end <= reserve_start {
        return Ok(U256::ZERO);
    }

    let delta = reserve_end - reserve_start;
    let one_minus_k = ONE.saturating_sub(k);

    // fair_amount = delta * (1 - k) ... the "no-slippage" portion
    let fair = delta * one_minus_k / ONE;

    // penalty = delta * k * target^2 / (reserve_start * reserve_end)
    let target_sq = target
        .checked_mul(target)
        .ok_or(QuoteError::Overflow { context: "dodo target^2" })?;
    let product = reserve_start
        .checked_mul(reserve_end)
        .ok_or(QuoteError::Overflow {
            context: "dodo reserve product",
        })?;
    let penalty = delta
        .checked_mul(k)
        .ok_or(QuoteError::Overflow { context: "dodo delta*k" })?
        / ONE
        * target_sq
        / product;

    Ok(fair + penalty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dodo_equilibrium() {
        let state = DodoPoolState {
            address: Address::ZERO,
            base_token: Address::with_last_byte(1),
            quote_token: Address::with_last_byte(2),
            base_reserve: U256::from(1_000_000_000_000u64),
            quote_reserve: U256::from(1_000_000_000_000u64),
            base_target: U256::from(1_000_000_000_000u64),
            quote_target: U256::from(1_000_000_000_000u64),
            r_state: R_ONE,
            k: ONE / U256::from(10u32), // k = 0.1
            lp_fee_rate: U256::from(3_000_000_000_000_000u64), // 0.3%
            mt_fee_rate: U256::ZERO,
        };
        let out = state
            .quote(state.base_token, U256::from(1_000_000u64))
            .unwrap();
        assert!(out > U256::ZERO);
    }
}
