use std::sync::Arc;
use std::time::Instant;

use alloy::sol;
use alloy_primitives::{Address, U256};
use anyhow::Result;
use tracing::{debug, warn};

use arb_core::types::*;
use arb_rpc::Endpoint;

use crate::pool_store::PoolStore;

sol! {
    #[sol(rpc)]
    interface IStateReader {
        struct V2State {
            address pool;
            address token0;
            address token1;
            uint112 reserve0;
            uint112 reserve1;
        }

        struct V3State {
            address pool;
            address token0;
            address token1;
            uint160 sqrtPriceX96;
            int24 tick;
            uint128 liquidity;
            uint24 fee;
            bool unlocked;
        }

        struct AlgebraState {
            address pool;
            address token0;
            address token1;
            uint160 sqrtPriceX96;
            int24 tick;
            uint128 liquidity;
            uint16 feeZto;
            uint16 feeOtz;
            bool unlocked;
        }

        struct AeroV2State {
            address pool;
            address token0;
            address token1;
            uint256 reserve0;
            uint256 reserve1;
            bool stable;
            uint256 decimals0;
            uint256 decimals1;
        }

        function readV2(address[] calldata pools) external view returns (V2State[] memory);
        function readV3(address[] calldata pools) external view returns (V3State[] memory);
        function readAlgebra(address[] calldata pools) external view returns (AlgebraState[] memory);
        function readAeroV2(address[] calldata pools) external view returns (AeroV2State[] memory);
    }
}

pub struct PoolConfig {
    pub address: Address,
    pub protocol: Protocol,
    pub fee_bps: u32,
}

pub struct StateRefresher {
    endpoint: Arc<Endpoint>,
    state_reader_addr: Address,
    pool_configs: Vec<PoolConfig>,
}

impl StateRefresher {
    const CHUNK_SIZE: usize = 50;

    pub fn new(
        endpoint: Arc<Endpoint>,
        state_reader_addr: Address,
        pool_configs: Vec<PoolConfig>,
    ) -> Self {
        Self {
            endpoint,
            state_reader_addr,
            pool_configs,
        }
    }

    /// Refresh all pool states from chain in a single batched call per protocol type.
    /// Returns the number of pools updated and elapsed time.
    pub async fn refresh(&self, store: &PoolStore) -> Result<(usize, std::time::Duration)> {
        let start = Instant::now();
        let mut updated = 0;

        let (v2_addrs, v3_addrs, algebra_addrs, aero_addrs) = self.partition_by_type();

        let provider = self.endpoint.provider();
        let reader = IStateReader::new(self.state_reader_addr, provider);

        let v2_chunks: Vec<_> = v2_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let v3_chunks: Vec<_> = v3_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let algebra_chunks: Vec<_> = algebra_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();
        let aero_chunks: Vec<_> = aero_addrs.chunks(Self::CHUNK_SIZE)
            .map(|c| c.to_vec())
            .collect();

        let (v2_results, v3_results, algebra_results, aero_results) = tokio::join!(
            async {
                let mut all = Vec::new();
                for chunk in &v2_chunks {
                    match reader.readV2(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "V2 chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &v3_chunks {
                    match reader.readV3(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "V3 chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &algebra_chunks {
                    match reader.readAlgebra(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "Algebra chunk read failed: {e}"),
                    }
                }
                all
            },
            async {
                let mut all = Vec::new();
                for chunk in &aero_chunks {
                    match reader.readAeroV2(chunk.clone()).call().await {
                        Ok(states) => all.extend(states),
                        Err(e) => warn!(chunk_size = chunk.len(), "AeroV2 chunk read failed: {e}"),
                    }
                }
                all
            },
        );

        for s in &v2_results {
            let fee_bps = self.fee_for_pool(&s.pool);
            store.update(
                s.pool,
                PoolState::V2(V2PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    reserve0: U256::from(s.reserve0),
                    reserve1: U256::from(s.reserve1),
                    fee_bps,
                }),
            );
            updated += 1;
        }

        for s in &v3_results {
            if s.sqrtPriceX96.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::V3(V3PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    sqrt_price_x96: U256::from(s.sqrtPriceX96),
                    tick: s.tick.as_i32(),
                    liquidity: s.liquidity,
                    fee: s.fee.to::<u32>(),
                }),
            );
            updated += 1;
        }

        for s in &algebra_results {
            if s.sqrtPriceX96.is_zero() {
                continue;
            }
            store.update(
                s.pool,
                PoolState::V3(V3PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    sqrt_price_x96: U256::from(s.sqrtPriceX96),
                    tick: s.tick.as_i32(),
                    liquidity: s.liquidity,
                    fee: s.feeZto as u32,
                }),
            );
            updated += 1;
        }

        for s in &aero_results {
            store.update(
                s.pool,
                PoolState::AeroV2(AeroV2PoolState {
                    address: s.pool,
                    token0: s.token0,
                    token1: s.token1,
                    reserve0: s.reserve0,
                    reserve1: s.reserve1,
                    stable: s.stable,
                    fee_bps: self.fee_for_pool(&s.pool),
                    decimals0: s.decimals0,
                    decimals1: s.decimals1,
                }),
            );
            updated += 1;
        }

        let block = self.endpoint.block_number().await.unwrap_or(0);
        store.set_block(block);

        let elapsed = start.elapsed();
        debug!(
            updated,
            elapsed_ms = elapsed.as_millis(),
            block,
            "State refresh completed"
        );

        Ok((updated, elapsed))
    }

    fn partition_by_type(&self) -> (Vec<Address>, Vec<Address>, Vec<Address>, Vec<Address>) {
        let mut v2 = Vec::new();
        let mut v3 = Vec::new();
        let mut algebra = Vec::new();
        let mut aero = Vec::new();

        for pc in &self.pool_configs {
            match pc.protocol {
                Protocol::UniswapV2 => v2.push(pc.address),
                Protocol::UniswapV3 | Protocol::UniswapV4 | Protocol::AerodromeSlipstream => v3.push(pc.address),
                Protocol::Algebra => algebra.push(pc.address),
                Protocol::AerodromeV2 => aero.push(pc.address),
                Protocol::PancakeStable | Protocol::Wombat | Protocol::DodoV2 => {
                    v2.push(pc.address);
                }
            }
        }

        (v2, v3, algebra, aero)
    }

    fn fee_for_pool(&self, pool: &Address) -> u32 {
        self.pool_configs
            .iter()
            .find(|pc| pc.address == *pool)
            .map(|pc| pc.fee_bps)
            .unwrap_or(30)
    }
}
