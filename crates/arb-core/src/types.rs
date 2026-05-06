use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    UniswapV2,
    UniswapV3,
    UniswapV4,
    PancakeStable,
    Wombat,
    DodoV2,
    Algebra,
    AerodromeV2,
    AerodromeSlipstream,
}

impl Protocol {
    /// Map to the on-chain contract's Protocol enum uint8 value.
    /// BscFlashArb.sol: {V3=0, V4=1, V2=2, PCS_STABLE=3, WOMBAT=4, DODO_V2=5, ALGEBRA=6}
    /// BaseFlashArb.sol: {V3=0, V4=1, V2=2, AERO_V2=3, AERO_SLIPSTREAM=4, ALGEBRA=5}
    pub fn to_contract_enum(&self, chain_id: u64) -> u8 {
        match chain_id {
            56 => match self {
                Self::UniswapV3 => 0,
                Self::UniswapV4 => 1,
                Self::UniswapV2 => 2,
                Self::PancakeStable => 3,
                Self::Wombat => 4,
                Self::DodoV2 => 5,
                Self::Algebra => 6,
                Self::AerodromeV2 | Self::AerodromeSlipstream => {
                    panic!("Aerodrome protocols are not supported on BSC (chain 56)")
                }
            },
            8453 => match self {
                Self::UniswapV3 => 0,
                Self::UniswapV4 => 1,
                Self::UniswapV2 => 2,
                Self::AerodromeV2 => 3,
                Self::AerodromeSlipstream => 4,
                Self::Algebra => 5,
                Self::PancakeStable | Self::Wombat | Self::DodoV2 => {
                    panic!("PCS Stable/Wombat/DODO are not supported on Base (chain 8453)")
                }
            },
            _ => panic!("Unknown chain_id {chain_id} for protocol enum mapping"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct V2PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,
}

#[derive(Debug, Clone)]
pub struct V3PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub fee: u32,
    /// Algebra pools have directional fees; `fee_otz` is the fee for token1->token0.
    /// For non-Algebra V3 pools this equals `fee`.
    pub fee_otz: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct CurvePoolState {
    pub address: Address,
    pub tokens: Vec<Address>,
    pub balances: Vec<U256>,
    pub amp: U256,
    pub fee: U256,
}

#[derive(Debug, Clone)]
pub struct WombatPoolState {
    pub address: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub cash_in: U256,
    pub cash_out: U256,
    pub liability_in: U256,
    pub liability_out: U256,
    pub amp: U256,
    pub haircut_rate: U256,
}

#[derive(Debug, Clone)]
pub struct DodoPoolState {
    pub address: Address,
    pub base_token: Address,
    pub quote_token: Address,
    pub base_reserve: U256,
    pub quote_reserve: U256,
    pub base_target: U256,
    pub quote_target: U256,
    pub r_state: u8,
    pub k: U256,
    pub lp_fee_rate: U256,
    pub mt_fee_rate: U256,
}

#[derive(Debug, Clone)]
pub struct AeroV2PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub stable: bool,
    pub fee_bps: u32,
    pub decimals0: U256,
    pub decimals1: U256,
}

#[derive(Debug, Clone)]
pub enum PoolState {
    V2(V2PoolState),
    V3(V3PoolState),
    Curve(CurvePoolState),
    Wombat(WombatPoolState),
    Dodo(DodoPoolState),
    AeroV2(AeroV2PoolState),
}
