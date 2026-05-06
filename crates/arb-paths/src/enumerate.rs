use std::collections::HashMap;

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
        }
    }

    /// Generate all valid 2-hop and 3-hop closed-loop paths.
    pub fn enumerate(&self) -> Vec<PathTemplate> {
        let mut paths = Vec::new();
        let mut id_counter: u32 = 0;

        // Build adjacency: token -> list of (pool_index, other_token)
        // Skip V4 pools — they are only used as the flash loan source, not as swap hops.
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

            let neighbors = match adjacency.get(&flash_token) {
                Some(n) => n,
                None => continue,
            };

            // 2-hop paths: flash_token -> mid -> flash_token
            for &(pool1_idx, mid_token) in neighbors {
                if mid_token == flash_token {
                    continue;
                }
                if let Some(mid_neighbors) = adjacency.get(&mid_token) {
                    for &(pool2_idx, end_token) in mid_neighbors {
                        if end_token != flash_token {
                            continue;
                        }
                        if pool1_idx == pool2_idx {
                            continue;
                        }

                        let path = PathTemplate {
                            id: id_counter,
                            flash_token,
                            flash_amount,
                            hops: vec![
                                make_hop(&self.pools[pool1_idx], flash_token, mid_token),
                                make_hop(&self.pools[pool2_idx], mid_token, flash_token),
                            ],
                        };
                        debug_assert!(path.is_valid());
                        paths.push(path);
                        id_counter += 1;
                    }
                }
            }

            // 3-hop paths: flash_token -> A -> B -> flash_token
            for &(pool1_idx, token_a) in neighbors {
                if token_a == flash_token {
                    continue;
                }
                if let Some(a_neighbors) = adjacency.get(&token_a) {
                    for &(pool2_idx, token_b) in a_neighbors {
                        if token_b == flash_token || token_b == token_a {
                            continue;
                        }
                        if pool2_idx == pool1_idx {
                            continue;
                        }
                        if let Some(b_neighbors) = adjacency.get(&token_b) {
                            for &(pool3_idx, end_token) in b_neighbors {
                                if end_token != flash_token {
                                    continue;
                                }
                                if pool3_idx == pool1_idx || pool3_idx == pool2_idx {
                                    continue;
                                }

                                let path = PathTemplate {
                                    id: id_counter,
                                    flash_token,
                                    flash_amount,
                                    hops: vec![
                                        make_hop(&self.pools[pool1_idx], flash_token, token_a),
                                        make_hop(&self.pools[pool2_idx], token_a, token_b),
                                        make_hop(&self.pools[pool3_idx], token_b, flash_token),
                                    ],
                                };
                                debug_assert!(path.is_valid());
                                paths.push(path);
                                id_counter += 1;
                            }
                        }
                    }
                }
            }
        }

        info!(
            total_paths = paths.len(),
            two_hop = paths.iter().filter(|p| p.num_hops() == 2).count(),
            three_hop = paths.iter().filter(|p| p.num_hops() == 3).count(),
            "Path enumeration complete"
        );

        paths
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
