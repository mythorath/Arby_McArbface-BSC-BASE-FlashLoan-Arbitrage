use alloy_primitives::{Address, U256};

use arb_core::types::*;
use arb_state::PoolStore;

/// Given a decoded pending swap, project the post-swap pool state.
/// This allows us to simulate arb paths against the state AFTER the pending tx lands.
pub fn project_post_state(
    pool_addr: Address,
    token_in: Address,
    amount_in: U256,
    store: &PoolStore,
) -> Option<PoolState> {
    let current = store.get(&pool_addr)?;

    match current {
        PoolState::V2(mut state) => {
            let is_token0_in = token_in == state.token0;
            let (reserve_in, reserve_out) = if is_token0_in {
                (&mut state.reserve0, &mut state.reserve1)
            } else {
                (&mut state.reserve1, &mut state.reserve0)
            };

            let amount_out = arb_core::v2::get_amount_out(
                amount_in,
                *reserve_in,
                *reserve_out,
                state.fee_bps,
            )
            .ok()?;

            *reserve_in = *reserve_in + amount_in;
            *reserve_out = reserve_out.checked_sub(amount_out)?;

            Some(PoolState::V2(state))
        }
        PoolState::AeroV2(mut state) => {
            // For volatile Aerodrome pools, same constant-product math
            if !state.stable {
                let is_token0_in = token_in == state.token0;
                let (reserve_in, reserve_out) = if is_token0_in {
                    (&mut state.reserve0, &mut state.reserve1)
                } else {
                    (&mut state.reserve1, &mut state.reserve0)
                };

                let fee_amount = amount_in * U256::from(state.fee_bps) / U256::from(10000u32);
                let amount_after_fee = amount_in - fee_amount;
                let amount_out = (amount_after_fee * *reserve_out) / (*reserve_in + amount_after_fee);

                *reserve_in = *reserve_in + amount_in;
                *reserve_out = reserve_out.checked_sub(amount_out)?;

                Some(PoolState::AeroV2(state))
            } else {
                // For stable pools, recompute is complex — for now skip projection
                None
            }
        }
        PoolState::V3(mut state) => {
            // For V3 pools, approximation: shift sqrt_price by the swap impact.
            // This is a rough estimate — exact would require tick-walking.
            // Good enough for "is there an arb opportunity" screening.
            let zero_for_one = token_in == state.token0;
            let l = U256::from(state.liquidity);
            if l.is_zero() {
                return None;
            }

            let q96 = U256::from(1u128) << 96;

            if zero_for_one {
                let product = amount_in * state.sqrt_price_x96 / q96;
                let denom: U256 = l + product;
                if denom.is_zero() {
                    return None;
                }
                state.sqrt_price_x96 = l * state.sqrt_price_x96 / denom;
            } else {
                let delta = amount_in * q96 / l;
                state.sqrt_price_x96 = state.sqrt_price_x96 + delta;
            }

            Some(PoolState::V3(state))
        }
        _ => None,
    }
}
