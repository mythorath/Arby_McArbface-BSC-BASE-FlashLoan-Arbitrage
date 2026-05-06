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

        // Run all reads in parallel
        let (v2_result, v3_result, algebra_result, aero_result) = tokio::join!(
            async {
                if v2_addrs.is_empty() { return Ok(vec![]); }
                reader.readV2(v2_addrs.clone()).call().await
            },
            async {
                if v3_addrs.is_empty() { return Ok(vec![]); }
                reader.readV3(v3_addrs.clone()).call().await
            },
            async {
                if algebra_addrs.is_empty() { return Ok(vec![]); }
                reader.readAlgebra(algebra_addrs.clone()).call().await
            },
            async {
                if aero_addrs.is_empty() { return Ok(vec![]); }
                reader.readAeroV2(aero_addrs.clone()).call().await
            },
        );

        if let Ok(states) = v2_result {
            for s in &states {
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
        } else if let Err(e) = &v2_result {
            warn!("V2 state read failed: {e}");
        }

        if let Ok(states) = v3_result {
            for s in &states {
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
        } else if let Err(e) = &v3_result {
            warn!("V3 state read failed: {e}");
        }

        if let Ok(states) = algebra_result {
            for s in &states {
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
        } else if let Err(e) = &algebra_result {
            warn!("Algebra state read failed: {e}");
        }

        if let Ok(states) = aero_result {
            for s in &states {
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
        } else if let Err(e) = &aero_result {
            warn!("AeroV2 state read failed: {e}");
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
