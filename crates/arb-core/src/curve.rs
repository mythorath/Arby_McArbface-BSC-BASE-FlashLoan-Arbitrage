use alloy_primitives::{Address, U256};

use crate::types::CurvePoolState;
use crate::{AmmQuoter, QuoteError};

impl AmmQuoter for CurvePoolState {
    fn quote(&self, token_in: Address, amount_in: U256) -> Result<U256, QuoteError> {
        if amount_in.is_zero() {
            return Err(QuoteError::ZeroInput);
        }

        let (i, j) = self.find_indices(token_in)?;
        get_dy(&self.balances, self.amp, i, j, amount_in, self.fee)
    }

    fn pool_id(&self) -> Address {
        self.address
    }
    fn token0(&self) -> Address {
        *self.tokens.first().unwrap_or(&Address::ZERO)
    }
    fn token1(&self) -> Address {
        *self.tokens.get(1).unwrap_or(&Address::ZERO)
    }
}

impl CurvePoolState {
    fn find_indices(&self, token_in: Address) -> Result<(usize, usize), QuoteError> {
        let i = self
            .tokens
            .iter()
            .position(|t| *t == token_in)
            .ok_or(QuoteError::NotInitialized)?;
        let j = if i == 0 { 1 } else { 0 };
        Ok((i, j))
    }
}

/// StableSwap invariant: A*n^n * sum(x_i) + D = A*D*n^n + D^(n+1) / (n^n * prod(x_i))
/// get_dy: compute output amount for a given input in a 2-token stable pool.
fn get_dy(
    balances: &[U256],
    amp: U256,
    i: usize,
    j: usize,
    dx: U256,
    fee: U256,
) -> Result<U256, QuoteError> {
    let n = U256::from(balances.len());
    let d = get_d(balances, amp)?;
    let ann = amp * n;

    let x_new_i = balances[i]
        .checked_add(dx)
        .ok_or(QuoteError::Overflow { context: "curve dx add" })?;

    // Newton's method to find y (balance[j] after swap)
    // from: Ann * sum + D = Ann * D + D^(n+1) / (n^n * prod)
    // solve for x_j given new x_i

    let mut s = U256::ZERO;
    let mut c = d;
    for k in 0..balances.len() {
        let x = if k == i {
            x_new_i
        } else if k == j {
            continue;
        } else {
            balances[k]
        };
        s = s.checked_add(x).ok_or(QuoteError::Overflow { context: "curve s" })?;
        c = c
            .checked_mul(d)
            .ok_or(QuoteError::Overflow { context: "curve c mul d" })?
            / (x * n);
    }

    c = c
        .checked_mul(d)
        .ok_or(QuoteError::Overflow { context: "curve c final" })?
        / (ann * n);

    let b = s + d / ann;

    // Newton iteration for y
    let mut y = d;
    for _ in 0..256 {
        let y_prev = y;
        let y_sq = y.checked_mul(y).ok_or(QuoteError::Overflow { context: "curve y^2" })?;
        y = (y_sq + c) / (y * U256::from(2u32) + b - d);
        let diff = if y > y_prev { y - y_prev } else { y_prev - y };
        if diff <= U256::from(1u32) {
            break;
        }
    }

    let dy = balances[j]
        .checked_sub(y)
        .ok_or(QuoteError::Overflow { context: "curve dy negative" })?;

    // Apply fee
    let fee_amount = dy * fee / U256::from(10_000_000_000u64);
    Ok(dy - fee_amount)
}

/// Compute the StableSwap invariant D using Newton's method.
fn get_d(balances: &[U256], amp: U256) -> Result<U256, QuoteError> {
    let n = U256::from(balances.len());
    let mut s = U256::ZERO;
    for b in balances {
        s = s.checked_add(*b).ok_or(QuoteError::Overflow { context: "curve D sum" })?;
    }
    if s.is_zero() {
        return Ok(U256::ZERO);
    }

    let ann = amp * n;
    let mut d = s;
    for _ in 0..256 {
        let mut d_p = d;
        for b in balances {
            d_p = d_p
                .checked_mul(d)
                .ok_or(QuoteError::Overflow { context: "curve D_P" })?
                / (*b * n);
        }
        let d_prev = d;
        let numerator = (ann * s + d_p * n) * d;
        let denominator = (ann - U256::from(1u32)) * d + (n + U256::from(1u32)) * d_p;
        d = numerator / denominator;
        let diff = if d > d_prev { d - d_prev } else { d_prev - d };
        if diff <= U256::from(1u32) {
            break;
        }
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stable_swap_equal_balances() {
        let balances = vec![
            U256::from(1_000_000_000_000u64), // 1M USDT (6 dec)
            U256::from(1_000_000_000_000u64), // 1M USDC (6 dec)
        ];
        let amp = U256::from(100u32);
        let fee = U256::from(4_000_000u64); // 0.04%
        let dx = U256::from(1_000_000u64); // 1 USDT

        let dy = get_dy(&balances, amp, 0, 1, dx, fee).unwrap();
        // For equal balances, output should be very close to input minus fee
        assert!(dy > U256::from(999_000u64));
        assert!(dy < U256::from(1_000_000u64));
    }

    #[test]
    fn test_d_invariant() {
        let balances = vec![U256::from(1_000_000u64), U256::from(1_000_000u64)];
        let amp = U256::from(100u32);
        let d = get_d(&balances, amp).unwrap();
        assert!(d > U256::ZERO);
        // For equal balances, D should be close to 2 * balance
        assert!(d >= U256::from(1_999_000u64));
    }
}
