use alloy_primitives::{Address, U256};
use tracing::trace;

use arb_core::types::*;
use arb_core::AmmQuoter;
use arb_paths::PathTemplate;
use arb_state::PoolStore;

#[derive(Debug, Clone)]
pub struct SimResult {
    pub path_id: u32,
    pub flash_token: Address,
    pub flash_amount: U256,
    pub final_amount: U256,
    pub gross_profit: U256,
    pub profit_bps: u32,
}

/// Evaluate a path template against the current pool state.
/// Returns Some(SimResult) if the path produces a gross profit, None otherwise.
pub fn evaluate_path(path: &PathTemplate, store: &PoolStore) -> Option<SimResult> {
    let mut current_amount = path.flash_amount;

    for hop in &path.hops {
        let pool_state = store.get(&hop.pool)?;

        let amount_out = match &pool_state {
            PoolState::V2(state) => state.quote(hop.token_in, current_amount).ok()?,
            PoolState::V3(state) => state.quote(hop.token_in, current_amount).ok()?,
            PoolState::Curve(state) => state.quote(hop.token_in, current_amount).ok()?,
            PoolState::Wombat(state) => state.quote(hop.token_in, current_amount).ok()?,
            PoolState::Dodo(state) => state.quote(hop.token_in, current_amount).ok()?,
            PoolState::AeroV2(state) => state.quote(hop.token_in, current_amount).ok()?,
        };

        if amount_out.is_zero() {
            return None;
        }

        current_amount = amount_out;
    }

    if current_amount <= path.flash_amount {
        return None;
    }

    let gross_profit = current_amount - path.flash_amount;
    let profit_bps = if !path.flash_amount.is_zero() {
        ((gross_profit * U256::from(10000u32)) / path.flash_amount)
            .try_into()
            .unwrap_or(u32::MAX)
    } else {
        0
    };

    trace!(
        path_id = path.id,
        profit_bps,
        gross_profit = %gross_profit,
        "Profitable path found"
    );

    Some(SimResult {
        path_id: path.id,
        flash_token: path.flash_token,
        flash_amount: path.flash_amount,
        final_amount: current_amount,
        gross_profit,
        profit_bps,
    })
}

/// Batch-evaluate all paths against current state, returning only profitable ones.
/// Uses rayon for parallel evaluation.
pub fn evaluate_all(paths: &[PathTemplate], store: &PoolStore) -> Vec<SimResult> {
    use rayon::prelude::*;

    let mut results: Vec<SimResult> = paths
        .par_iter()
        .filter_map(|path| evaluate_path(path, store))
        .collect();

    results.sort_by(|a, b| b.gross_profit.cmp(&a.gross_profit));
    results
}
