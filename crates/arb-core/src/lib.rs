pub mod v2;
pub mod v3;
pub mod curve;
pub mod wombat;
pub mod dodo;
pub mod aerodrome;
pub mod types;

use alloy_primitives::{Address, U256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QuoteError {
    #[error("zero liquidity in pool {pool}")]
    ZeroLiquidity { pool: Address },
    #[error("zero input amount")]
    ZeroInput,
    #[error("math overflow in {context}")]
    Overflow { context: &'static str },
    #[error("tick out of range: {tick}")]
    TickOutOfRange { tick: i32 },
    #[error("insufficient coverage ratio")]
    InsufficientCoverage,
    #[error("pool not initialized")]
    NotInitialized,
}

pub trait AmmQuoter: Send + Sync {
    fn quote(
        &self,
        token_in: Address,
        amount_in: U256,
    ) -> Result<U256, QuoteError>;

    fn pool_id(&self) -> Address;

    fn token0(&self) -> Address;
    fn token1(&self) -> Address;
}
