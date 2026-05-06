use alloy_primitives::Address;
use arb_core::types::Protocol;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HopTemplate {
    pub protocol: Protocol,
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathTemplate {
    pub id: u32,
    /// The token we borrow via flash loan and must repay
    pub flash_token: Address,
    /// Amount to borrow (in smallest unit)
    pub flash_amount: alloy_primitives::U256,
    /// Ordered swap hops. After all hops, we must hold >= flash_amount of flash_token.
    pub hops: Vec<HopTemplate>,
}

impl PathTemplate {
    pub fn num_hops(&self) -> usize {
        self.hops.len()
    }

    /// Validates that the path is a closed loop: token_out of last hop == flash_token
    pub fn is_valid(&self) -> bool {
        if self.hops.is_empty() {
            return false;
        }
        if self.hops[0].token_in != self.flash_token {
            return false;
        }
        for window in self.hops.windows(2) {
            if window[0].token_out != window[1].token_in {
                return false;
            }
        }
        self.hops.last().unwrap().token_out == self.flash_token
    }
}
