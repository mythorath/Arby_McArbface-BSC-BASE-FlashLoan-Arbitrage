use std::collections::{HashMap, HashSet};

use alloy_primitives::{Address, U256};
use tracing::info;

use arb_core::types::Protocol;

use crate::template::{HopTemplate, PathTemplate};

/// Configuration for a pool available for path enumeration.
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub address: Address,
    pub protocol: Protocol,
    pub token0: Address,
    pub token1: Address,
}

pub struct PathEnumerator {
    pools: Vec<PoolInfo>,
    flash_tokens: Vec<Address>,
    flash_amounts: HashMap<Address, U256>,
    max_hops: usize,
    max_paths_per_flash_token: usize,
    max_paths_through_token: usize,
}

impl PathEnumerator {
    pub fn new(
        pools: Vec<PoolInfo>,
        flash_tokens: Vec<Address>,
        flash_amounts: HashMap<Address, U256>,
    ) -> Self {
        Self {
            pools,
            flash_tokens,
            flash_amounts,
            max_hops: 4,
            max_paths_per_flash_token: 25000,
            max_paths_through_token: 200,
        }
    }

    pub fn with_limits(
        mut self,
        max_hops: usize,
        max_paths_per_flash_token: usize,
        max_paths_through_token: usize,
    ) -> Self {
        self.max_hops = max_hops;
        self.max_paths_per_flash_token = max_paths_per_flash_token;
        self.max_paths_through_token = max_paths_through_token;
        self
    }

    pub fn enumerate(&self) -> Vec<PathTemplate> {
        let mut paths = Vec::new();
        let mut id_counter: u32 = 0;

        let mut adjacency: HashMap<Address, Vec<(usize, Address)>> = HashMap::new();
        for (idx, pool) in self.pools.iter().enumerate() {
            if pool.protocol == Protocol::UniswapV4 {
                continue;
            }
            adjacency
                .entry(pool.token0)
                .or_default()
                .push((idx, pool.token1));
            adjacency
                .entry(pool.token1)
                .or_default()
                .push((idx, pool.token0));
        }

        for &flash_token in &self.flash_tokens {
            let flash_amount = self
                .flash_amounts
                .get(&flash_token)
                .copied()
                .unwrap_or(U256::from(5_000_000u64));

            let mut flash_paths = Vec::new();
            let mut token_path_counts: HashMap<Address, usize> = HashMap::new();

            Self::dfs(
                &adjacency,
                &self.pools,
                flash_token,
                flash_token,
                flash_amount,
                &mut Vec::new(),
                &mut HashSet::new(),
                &mut HashSet::new(),
                0,
                self.max_hops,
                self.max_paths_per_flash_token,
                self.max_paths_through_token,
                &mut flash_paths,
                &mut token_path_counts,
                &mut id_counter,
            );

            let two = flash_paths.iter().filter(|p| p.num_hops() == 2).count();
            let three = flash_paths.iter().filter(|p| p.num_hops() == 3).count();
            let four_plus = flash_paths.iter().filter(|p| p.num_hops() >= 4).count();

            info!(
                flash_token = %flash_token,
                total = flash_paths.len(),
                two_hop = two,
                three_hop = three,
                four_plus_hop = four_plus,
                "Paths enumerated for flash token"
            );

            paths.extend(flash_paths);
        }

        info!(
            total_paths = paths.len(),
            two_hop = paths.iter().filter(|p| p.num_hops() == 2).count(),
            three_hop = paths.iter().filter(|p| p.num_hops() == 3).count(),
            four_plus = paths.iter().filter(|p| p.num_hops() >= 4).count(),
            max_hops = self.max_hops,
            "Path enumeration complete"
        );

        paths
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs(
        adjacency: &HashMap<Address, Vec<(usize, Address)>>,
        pools: &[PoolInfo],
        flash_token: Address,
        current_token: Address,
        flash_amount: U256,
        hops: &mut Vec<HopTemplate>,
        used_pools: &mut HashSet<usize>,
        visited_tokens: &mut HashSet<Address>,
        depth: usize,
        max_hops: usize,
        max_paths: usize,
        max_per_token: usize,
        results: &mut Vec<PathTemplate>,
        token_counts: &mut HashMap<Address, usize>,
        id_counter: &mut u32,
    ) {
        if results.len() >= max_paths {
            return;
        }

        if depth > 0 && current_token == flash_token {
            let path = PathTemplate {
                id: *id_counter,
                flash_token,
                flash_amount,
                hops: hops.clone(),
            };
            debug_assert!(path.is_valid());
            results.push(path);
            *id_counter += 1;
            return;
        }

        if depth >= max_hops {
            return;
        }

        let neighbors = match adjacency.get(&current_token) {
            Some(n) => n,
            None => return,
        };

        for &(pool_idx, next_token) in neighbors {
            if used_pools.contains(&pool_idx) {
                continue;
            }

            if next_token != flash_token && visited_tokens.contains(&next_token) {
                continue;
            }

            if next_token != flash_token {
                let count = token_counts.get(&next_token).copied().unwrap_or(0);
                if count >= max_per_token {
                    continue;
                }
            }

            hops.push(make_hop(&pools[pool_idx], current_token, next_token));
            used_pools.insert(pool_idx);
            if next_token != flash_token {
                visited_tokens.insert(next_token);
                *token_counts.entry(next_token).or_insert(0) += 1;
            }

            Self::dfs(
                adjacency, pools, flash_token, next_token, flash_amount,
                hops, used_pools, visited_tokens,
                depth + 1, max_hops, max_paths, max_per_token,
                results, token_counts, id_counter,
            );

            hops.pop();
            used_pools.remove(&pool_idx);
            if next_token != flash_token {
                visited_tokens.remove(&next_token);
            }
        }
    }
}

fn make_hop(pool: &PoolInfo, token_in: Address, token_out: Address) -> HopTemplate {
    HopTemplate {
        protocol: pool.protocol,
        pool: pool.address,
        token_in,
        token_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(b: u8) -> Address {
        Address::with_last_byte(b)
    }

    #[test]
    fn test_enumerate_triangle() {
        let pools = vec![
            PoolInfo {
                address: addr(10),
                protocol: Protocol::UniswapV2,
                token0: addr(1),
                token1: addr(2),
            },
            PoolInfo {
                address: addr(11),
                protocol: Protocol::UniswapV2,
                token0: addr(2),
                token1: addr(3),
            },
            PoolInfo {
                address: addr(12),
                protocol: Protocol::UniswapV2,
                token0: addr(1),
                token1: addr(3),
            },
        ];

        let mut amounts = HashMap::new();
        amounts.insert(addr(1), U256::from(1_000_000u64));

        let enumerator = PathEnumerator::new(pools, vec![addr(1)], amounts);
        let paths = enumerator.enumerate();

        assert!(!paths.is_empty());
        for path in &paths {
            assert!(path.is_valid());
        }
    }
}
