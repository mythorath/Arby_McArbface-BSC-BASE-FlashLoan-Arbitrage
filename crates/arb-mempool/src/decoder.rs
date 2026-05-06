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

fn decode_v3_exact_input_single(data: &[u8]) -> Option<(Address, Address, U256)> {
    if data.len() < 7 * 32 { return None; }
    let offset: usize = read_u256(data, 0)?.try_into().ok()?;
    let base = offset / 32;
    let token_in = read_addr(data, base)?;
    let token_out = read_addr(data, base + 1)?;
    let amount_in = read_u256(data, base + 4)?;
    Some((token_in, token_out, amount_in))
}

/// Decode Universal Router `execute(bytes commands, bytes[] inputs, uint256 deadline)`.
/// We look at the first command byte; 0x00 = V3_SWAP_EXACT_IN, 0x08 = V2_SWAP_EXACT_IN.
fn decode_universal_router(data: &[u8]) -> Option<(Address, Address, U256)> {
    if data.len() < 4 * 32 { return None; }

    let commands_offset: usize = read_u256(data, 0)?.try_into().ok()?;
    let inputs_offset: usize = read_u256(data, 1)?.try_into().ok()?;

    let cmd_len_offset = commands_offset / 32;
    let cmd_len: usize = read_u256(data, cmd_len_offset)?.try_into().ok()?;
    if cmd_len == 0 { return None; }

    let cmd_data_start = commands_offset + 32;
    if data.len() < cmd_data_start + cmd_len { return None; }
    let first_cmd = data[cmd_data_start] & 0x1f;

    let inputs_len_offset = inputs_offset / 32;
    let inputs_len: usize = read_u256(data, inputs_len_offset)?.try_into().ok()?;
    if inputs_len == 0 { return None; }

    let first_input_ptr_offset = inputs_offset + 32;
    if data.len() < first_input_ptr_offset + 32 { return None; }
    let first_input_rel: usize = U256::from_be_slice(
        &data[first_input_ptr_offset..first_input_ptr_offset + 32]
    ).try_into().ok()?;
    let first_input_abs = inputs_offset + 32 + first_input_rel;

    let input_len_offset = first_input_abs;
    if data.len() < input_len_offset + 32 { return None; }
    let input_len: usize = U256::from_be_slice(
        &data[input_len_offset..input_len_offset + 32]
    ).try_into().ok()?;
    let input_data_start = input_len_offset + 32;
    if data.len() < input_data_start + input_len { return None; }
    let input_data = &data[input_data_start..input_data_start + input_len];

    match first_cmd {
        0x00 => {
            // V3_SWAP_EXACT_IN: (address recipient, uint256 amountIn, uint256 amountOutMin, bytes path, bool payerIsUser)
            if input_data.len() < 4 * 32 { return None; }
            let amount_in = read_u256(input_data, 1)?;
            let path_offset: usize = read_u256(input_data, 3)?.try_into().ok()?;
            let path_len_off = path_offset / 32;
            let path_len: usize = read_u256(input_data, path_len_off)?.try_into().ok()?;
            if path_len < 43 { return None; } // min: 20 + 3 + 20
            let path_start = path_offset + 32;
            if input_data.len() < path_start + path_len { return None; }
            let path_bytes = &input_data[path_start..path_start + path_len];
            let token_in = Address::from_slice(&path_bytes[..20]);
            let token_out = Address::from_slice(&path_bytes[path_len - 20..]);
            Some((token_in, token_out, amount_in))
        }
        0x08 => {
            // V2_SWAP_EXACT_IN: (address recipient, uint256 amountIn, uint256 amountOutMin, address[] path, bool payerIsUser)
            if input_data.len() < 4 * 32 { return None; }
            let amount_in = read_u256(input_data, 1)?;
            let path_offset: usize = read_u256(input_data, 3)?.try_into().ok()?;
            let path_len_off = path_offset / 32;
            let path_len: usize = read_u256(input_data, path_len_off)?.try_into().ok()?;
            if path_len < 2 { return None; }
            let token_in = read_addr(input_data, path_len_off + 1)?;
            let token_out = read_addr(input_data, path_len_off + path_len)?;
            Some((token_in, token_out, amount_in))
        }
        _ => None,
    }
}

/// Decode 1inch/OpenOcean style swap with SwapDescription struct.
/// Layout: (address executor/caller, (address srcToken, address dstToken, address srcReceiver,
///          address dstReceiver, uint256 amount, ...) desc, bytes permit/data, bytes data)
fn decode_swap_description(data: &[u8]) -> Option<(Address, Address, U256)> {
    if data.len() < 7 * 32 { return None; }
    let desc_offset: usize = read_u256(data, 1)?.try_into().ok()?;
    let base = desc_offset / 32;
    let src_token = read_addr(data, base)?;
    let dst_token = read_addr(data, base + 1)?;
    let amount = read_u256(data, base + 4)?;
    Some((src_token, dst_token, amount))
}

/// Decode 1inch unoswapTo: (address to, address srcToken, uint256 amount, uint256 minReturn, uint256[] pools)
fn decode_unoswap(data: &[u8]) -> Option<(Address, Address, U256)> {
    if data.len() < 5 * 32 { return None; }
    let src_token = read_addr(data, 1)?;
    let amount = read_u256(data, 2)?;
    Some((src_token, Address::ZERO, amount))
}

impl TxDecoder {
    pub fn new() -> Self {
        Self {
            known_selectors: vec![
                // V2-style routers
                ([0x38, 0xed, 0x17, 0x39], "UniV2_swapExactTokensForTokens"),
                ([0x7f, 0xf3, 0x6a, 0xb5], "UniV2_swapExactETHForTokens"),
                ([0x18, 0xcb, 0xaf, 0xe5], "UniV2_swapExactTokensForETH"),
                ([0x62, 0x58, 0xf5, 0xf0], "Aero_swapExactTokensForTokens"),
                // V3 routers
                ([0x41, 0x4b, 0xf3, 0x89], "UniV3_exactInputSingle"),
                ([0xb8, 0x58, 0x18, 0x3f], "UniV3_exactInput"),
                // Universal Router
                ([0x35, 0x93, 0x56, 0x4c], "UniversalRouter_execute"),
                ([0x3f, 0x62, 0x19, 0x2f], "UniversalRouter_execute_deadline"),
                // PancakeSwap SmartRouter
                ([0x5c, 0x11, 0xd7, 0x95], "PCS_swap"),
                // 1inch v5/v6
                ([0x12, 0xaa, 0x3c, 0xaf], "1inch_swap"),
                ([0xf7, 0x8d, 0xc2, 0x53], "1inch_unoswapTo"),
                ([0xe2, 0xc9, 0x51, 0x59], "1inch_unoswap"),
                ([0x07, 0xed, 0x23, 0x79], "1inch_v6_swap"),
                // KyberSwap
                ([0xe2, 0x1f, 0xd0, 0xe9], "KyberSwap_swap"),
                // OpenOcean
                ([0x90, 0x41, 0x1a, 0x32], "OpenOcean_swap"),
                // OKX DEX
                ([0x36, 0xb1, 0xa1, 0xbc], "OKX_swap"),
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
            "UniversalRouter_execute" | "UniversalRouter_execute_deadline" => {
                decode_universal_router(data).map(|(a, b, c)| (Some(a), Some(b), Some(c))).unwrap_or_default()
            }
            "1inch_swap" | "1inch_v6_swap" | "OpenOcean_swap" | "OKX_swap" | "KyberSwap_swap" => {
                decode_swap_description(data).map(|(a, b, c)| (Some(a), Some(b), Some(c))).unwrap_or_default()
            }
            "1inch_unoswapTo" | "1inch_unoswap" => {
                decode_unoswap(data).map(|(a, b, c)| {
                    let out = if b == Address::ZERO { None } else { Some(b) };
                    (Some(a), out, Some(c))
                }).unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    fn pad_u256(val: U256) -> [u8; 32] {
        let bytes: [u8; 32] = val.to_be_bytes();
        bytes
    }

    fn pad_addr(addr: Address) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_slice());
        word
    }

    #[test]
    fn test_decode_known_v2_selector() {
        let decoder = TxDecoder::new();
        let selector = [0x38, 0xed, 0x17, 0x39];
        let token_in = address!("1111111111111111111111111111111111111111");
        let token_out = address!("2222222222222222222222222222222222222222");
        let amount_in = U256::from(1000u64);

        let mut data = Vec::new();
        data.extend_from_slice(&selector);
        data.extend_from_slice(&pad_u256(amount_in));                // word 0: amountIn
        data.extend_from_slice(&pad_u256(U256::from(1u64)));         // word 1: amountOutMin
        data.extend_from_slice(&pad_u256(U256::from(160u64)));       // word 2: path offset = 5*32
        data.extend_from_slice(&pad_addr(Address::ZERO));            // word 3: to
        data.extend_from_slice(&pad_u256(U256::from(9999999u64)));   // word 4: deadline
        data.extend_from_slice(&pad_u256(U256::from(2u64)));         // word 5: path length
        data.extend_from_slice(&pad_addr(token_in));                 // word 6: path[0]
        data.extend_from_slice(&pad_addr(token_out));                // word 7: path[1]

        let decoded = decoder.decode(Address::ZERO, &data).unwrap();
        assert_eq!(decoded.router, "UniV2_swapExactTokensForTokens");
        assert_eq!(decoded.token_in, Some(token_in));
        assert_eq!(decoded.token_out, Some(token_out));
        assert_eq!(decoded.amount_in, Some(amount_in));
    }

    #[test]
    fn test_decode_unknown_selector() {
        let decoder = TxDecoder::new();
        let input = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00, 0x00, 0x00];
        assert!(decoder.decode(Address::ZERO, &input).is_none());
    }

    #[test]
    fn test_decode_too_short() {
        let decoder = TxDecoder::new();
        assert!(decoder.decode(Address::ZERO, &[0x38, 0xed]).is_none());
        assert!(decoder.decode(Address::ZERO, &[]).is_none());
    }

    #[test]
    fn test_decode_v3_selector() {
        let decoder = TxDecoder::new();
        let selector = [0x41, 0x4b, 0xf3, 0x89];
        let token_in = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let token_out = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let amount_in = U256::from(5000u64);

        let mut data = Vec::new();
        data.extend_from_slice(&selector);
        data.extend_from_slice(&pad_u256(U256::from(32u64)));  // word 0: offset to struct = 32
        data.extend_from_slice(&pad_addr(token_in));           // word 1: tokenIn
        data.extend_from_slice(&pad_addr(token_out));          // word 2: tokenOut
        data.extend_from_slice(&pad_u256(U256::from(3000u64)));// word 3: fee
        data.extend_from_slice(&pad_addr(Address::ZERO));      // word 4: recipient
        data.extend_from_slice(&pad_u256(amount_in));          // word 5: amountIn
        data.extend_from_slice(&pad_u256(U256::from(1u64)));   // word 6: amountOutMin
        data.extend_from_slice(&pad_u256(U256::ZERO));         // word 7: sqrtPriceLimitX96

        let decoded = decoder.decode(Address::ZERO, &data).unwrap();
        assert_eq!(decoded.router, "UniV3_exactInputSingle");
        assert_eq!(decoded.token_in, Some(token_in));
        assert_eq!(decoded.token_out, Some(token_out));
        assert_eq!(decoded.amount_in, Some(amount_in));
    }

    #[test]
    fn test_read_u256_basic() {
        let val = U256::from(42u64);
        let buf = pad_u256(val);
        let mut data = vec![0u8; 32];
        data.extend_from_slice(&buf);
        assert_eq!(read_u256(&data, 0), Some(U256::ZERO));
        assert_eq!(read_u256(&data, 1), Some(val));
        assert_eq!(read_u256(&data, 2), None);
    }

    #[test]
    fn test_read_addr_basic() {
        let addr = address!("1234567890abcdef1234567890abcdef12345678");
        let buf = pad_addr(addr);
        let result = read_addr(&buf, 0).unwrap();
        assert_eq!(result, addr);
    }

    #[test]
    fn test_decoder_default() {
        let d1 = TxDecoder::new();
        let d2 = TxDecoder::default();
        assert_eq!(d1.known_selectors.len(), d2.known_selectors.len());
        for (a, b) in d1.known_selectors.iter().zip(d2.known_selectors.iter()) {
            assert_eq!(a.0, b.0);
            assert_eq!(a.1, b.1);
        }
    }
}
