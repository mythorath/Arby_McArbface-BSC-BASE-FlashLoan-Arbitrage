use alloy_primitives::{Address, U256};
use tracing::trace;

pub struct TxDecoder {
    known_selectors: Vec<([u8; 4], &'static str)>,
}

#[derive(Debug, Clone)]
pub struct DecodedSwap {
    pub router: &'static str,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub amount_in: Option<U256>,
    pub pools_touched: Vec<Address>,
}

fn read_u256(data: &[u8], word: usize) -> Option<U256> {
    let start = word * 32;
    if data.len() < start + 32 { return None; }
    Some(U256::from_be_slice(&data[start..start + 32]))
}

fn read_addr(data: &[u8], word: usize) -> Option<Address> {
    let start = word * 32 + 12;
    if data.len() < start + 20 { return None; }
    Some(Address::from_slice(&data[start..start + 20]))
}

/// Decode V2-style `swapExactTokensForTokens(amountIn, amountOutMin, path[], to, deadline)`.
/// The `path` array is at dynamic offset word 2. Extract token_in, token_out, amount_in.
fn decode_v2_swap(data: &[u8]) -> Option<(Address, Address, U256)> {
    if data.len() < 5 * 32 { return None; }
    let amount_in = read_u256(data, 0)?;
    let path_offset = read_u256(data, 2)?.try_into().ok().unwrap_or(0usize);
    let path_len_offset = path_offset / 32;
    let path_len: usize = read_u256(data, path_len_offset)?.try_into().ok()?;
    if path_len < 2 { return None; }
    let token_in = read_addr(data, path_len_offset + 1)?;
    let token_out = read_addr(data, path_len_offset + path_len)?;
    Some((token_in, token_out, amount_in))
}

/// Decode V3 `exactInputSingle((tokenIn, tokenOut, fee, recipient, amountIn, amountOutMin, sqrtPriceLimitX96))`.
fn decode_v3_exact_input_single(data: &[u8]) -> Option<(Address, Address, U256)> {
    if data.len() < 7 * 32 { return None; }
    // The params are a struct packed as sequential words (via tuple offset)
    let offset: usize = read_u256(data, 0)?.try_into().ok()?;
    let base = offset / 32;
    let token_in = read_addr(data, base)?;
    let token_out = read_addr(data, base + 1)?;
    let amount_in = read_u256(data, base + 4)?;
    Some((token_in, token_out, amount_in))
}

impl TxDecoder {
    pub fn new() -> Self {
        Self {
            known_selectors: vec![
                ([0x38, 0xed, 0x17, 0x39], "UniV2_swapExactTokensForTokens"),
                ([0x7f, 0xf3, 0x6a, 0xb5], "UniV2_swapExactETHForTokens"),
                ([0x18, 0xcb, 0xaf, 0xe5], "UniV2_swapExactTokensForETH"),
                ([0x41, 0x4b, 0xf3, 0x89], "UniV3_exactInputSingle"),
                ([0xb8, 0x58, 0x18, 0x3f], "UniV3_exactInput"),
                ([0x35, 0x93, 0x56, 0x4c], "UniversalRouter_execute"),
                ([0x5c, 0x11, 0xd7, 0x95], "PCS_swap"),
                ([0x12, 0xaa, 0x3c, 0xaf], "1inch_swap"),
                ([0x62, 0x58, 0xf5, 0xf0], "Aero_swapExactTokensForTokens"),
            ],
        }
    }

    pub fn decode(&self, to: Address, input: &[u8]) -> Option<DecodedSwap> {
        if input.len() < 4 {
            return None;
        }

        let selector: [u8; 4] = input[..4].try_into().ok()?;

        let router_name = self
            .known_selectors
            .iter()
            .find(|(sel, _)| sel == &selector)
            .map(|(_, name)| *name)?;

        trace!(router = router_name, to = %to, "Decoded pending swap tx");

        let data = &input[4..];
        let (token_in, token_out, amount_in) = match router_name {
            "UniV2_swapExactTokensForTokens" | "UniV2_swapExactTokensForETH" |
            "Aero_swapExactTokensForTokens" => {
                decode_v2_swap(data).map(|(a, b, c)| (Some(a), Some(b), Some(c))).unwrap_or_default()
            }
            "UniV3_exactInputSingle" => {
                decode_v3_exact_input_single(data).map(|(a, b, c)| (Some(a), Some(b), Some(c))).unwrap_or_default()
            }
            _ => (None, None, None),
        };

        Some(DecodedSwap {
            router: router_name,
            token_in,
            token_out,
            amount_in,
            pools_touched: vec![],
        })
    }
}

impl Default for TxDecoder {
    fn default() -> Self {
        Self::new()
    }
}
