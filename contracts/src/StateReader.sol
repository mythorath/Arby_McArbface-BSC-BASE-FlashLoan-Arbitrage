// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title StateReader
 * @notice Batched view-only contract for reading pool state in a single eth_call.
 *         Returns packed data for V2, V3, Aerodrome, Algebra, PCS Stable, Wombat,
 *         and DODO V2 pools so the Rust scanner can refresh all pool state in one
 *         round trip.
 *
 * Deployed on both BSC and Base. No state, no owner, pure view helper.
 */

interface IV2Like {
    function getReserves() external view returns (uint112, uint112, uint32);
    function token0() external view returns (address);
    function token1() external view returns (address);
}

interface IV3Like {
    function slot0() external view returns (
        uint160 sqrtPriceX96, int24 tick, uint16 observationIndex,
        uint16 observationCardinality, uint16 observationCardinalityNext,
        uint8 feeProtocol, bool unlocked
    );
    function liquidity() external view returns (uint128);
    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
}

interface IAlgebraLike {
    function globalState() external view returns (
        uint160 price, int24 tick, uint16 feeZto, uint16 feeOtz,
        uint16 timepointIndex, uint8 communityFeeToken0, uint8 communityFeeToken1, bool unlocked
    );
    function liquidity() external view returns (uint128);
    function token0() external view returns (address);
    function token1() external view returns (address);
}

interface IAeroV2Like {
    function getReserves() external view returns (uint256, uint256, uint256);
    function stable() external view returns (bool);
    function token0() external view returns (address);
    function token1() external view returns (address);
}

contract StateReader {

    // ──────── V2 ────────

    struct V2State {
        address pool;
        address token0;
        address token1;
        uint112 reserve0;
        uint112 reserve1;
        uint32 fee;
    }

    // ──────── V3 ────────

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

    // ──────── Algebra ────────

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

    // ──────── Aerodrome V2 ────────

    struct AeroV2State {
        address pool;
        address token0;
        address token1;
        uint256 reserve0;
        uint256 reserve1;
        bool stable;
        uint256 decimals0;
        uint256 decimals1;
        uint32 fee;
    }

    // ──────── PancakeStable (Curve-style) ────────

    struct PcsStableState {
        address pool;
        address token0;
        address token1;
        uint256 balance0;
        uint256 balance1;
        uint256 A;
        uint256 fee;
        uint256 adminFee;
    }

    // ──────── DODO V2 ────────

    struct DodoV2State {
        address pool;
        address baseToken;
        address quoteToken;
        uint256 baseReserve;
        uint256 quoteReserve;
        uint256 baseTarget;
        uint256 quoteTarget;
        uint8 rState;
        uint256 k;
        uint256 lpFeeRate;
        uint256 mtFeeRate;
    }

    // ──────── Wombat ────────

    struct WombatState {
        address pool;
        address token0;
        address token1;
        uint256 cash0;
        uint256 cash1;
        uint256 liability0;
        uint256 liability1;
        uint256 ampFactor;
        uint256 haircutRate;
    }

    // ═══════ Readers ═══════

    function readV2(address[] calldata pools) external view returns (V2State[] memory results) {
        results = new V2State[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            try IV2Like(pools[i]).token0() returns (address t0) { results[i].token0 = t0; } catch {}
            try IV2Like(pools[i]).token1() returns (address t1) { results[i].token1 = t1; } catch {}
            try IV2Like(pools[i]).getReserves() returns (uint112 r0, uint112 r1, uint32) {
                results[i].reserve0 = r0;
                results[i].reserve1 = r1;
            } catch {}

            // Try common fee getter patterns; leave 0 as sentinel for "use Rust fallback"
            (bool feeOk, bytes memory feeData) = pools[i].staticcall(abi.encodeWithSignature("swapFee()"));
            if (feeOk && feeData.length >= 32) {
                uint256 raw;
                assembly { raw := mload(add(feeData, 32)) }
                if (raw > 0 && raw <= 10000) {
                    results[i].fee = uint32(raw);
                }
            }
        }
    }

    function readV3(address[] calldata pools) external view returns (V3State[] memory results) {
        results = new V3State[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            try IV3Like(pools[i]).token0() returns (address t0) { results[i].token0 = t0; } catch {}
            try IV3Like(pools[i]).token1() returns (address t1) { results[i].token1 = t1; } catch {}
            try IV3Like(pools[i]).fee() returns (uint24 f) { results[i].fee = f; } catch {}
            (bool ok, bytes memory data) = pools[i].staticcall(abi.encodeWithSignature("slot0()"));
            if (ok && data.length >= 64) {
                uint256 word0;
                uint256 word1;
                assembly {
                    word0 := mload(add(data, 32))
                    word1 := mload(add(data, 64))
                }
                results[i].sqrtPriceX96 = uint160(word0);
                results[i].tick = int24(int256(word1));
                results[i].unlocked = true;
                if (data.length >= 224) {
                    uint256 lastWord;
                    assembly {
                        lastWord := mload(add(data, 224))
                    }
                    results[i].unlocked = (lastWord != 0);
                }
            }
            try IV3Like(pools[i]).liquidity() returns (uint128 liq) {
                results[i].liquidity = liq;
            } catch {}
        }
    }

    function readAlgebra(address[] calldata pools) external view returns (AlgebraState[] memory results) {
        results = new AlgebraState[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            try IAlgebraLike(pools[i]).token0() returns (address t0) { results[i].token0 = t0; } catch {}
            try IAlgebraLike(pools[i]).token1() returns (address t1) { results[i].token1 = t1; } catch {}
            (bool ok, bytes memory data) = pools[i].staticcall(abi.encodeWithSignature("globalState()"));
            if (ok && data.length >= 128) {
                uint256 word0;
                uint256 word1;
                uint256 word2;
                uint256 word3;
                assembly {
                    word0 := mload(add(data, 32))
                    word1 := mload(add(data, 64))
                    word2 := mload(add(data, 96))
                    word3 := mload(add(data, 128))
                }
                results[i].sqrtPriceX96 = uint160(word0);
                results[i].tick = int24(int256(word1));
                results[i].feeZto = uint16(word2);
                results[i].feeOtz = uint16(word3);
                results[i].unlocked = true;
                if (data.length >= 256) {
                    uint256 lastWord;
                    assembly {
                        lastWord := mload(add(data, 256))
                    }
                    results[i].unlocked = (lastWord != 0);
                }
            }
            try IAlgebraLike(pools[i]).liquidity() returns (uint128 liq) {
                results[i].liquidity = liq;
            } catch {}
        }
    }

    function readAeroV2(address[] calldata pools) external view returns (AeroV2State[] memory results) {
        results = new AeroV2State[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            try IAeroV2Like(pools[i]).token0() returns (address t0) { results[i].token0 = t0; } catch {}
            try IAeroV2Like(pools[i]).token1() returns (address t1) { results[i].token1 = t1; } catch {}
            try IAeroV2Like(pools[i]).stable() returns (bool s) { results[i].stable = s; } catch {}
            try IAeroV2Like(pools[i]).getReserves() returns (uint256 r0, uint256 r1, uint256) {
                results[i].reserve0 = r0;
                results[i].reserve1 = r1;
            } catch {}

            (bool ok0, bytes memory d0) = results[i].token0.staticcall(abi.encodeWithSignature("decimals()"));
            results[i].decimals0 = (ok0 && d0.length >= 32) ? 10 ** uint256(bytes32(d0)) : 1e18;

            (bool ok1, bytes memory d1) = results[i].token1.staticcall(abi.encodeWithSignature("decimals()"));
            results[i].decimals1 = (ok1 && d1.length >= 32) ? 10 ** uint256(bytes32(d1)) : 1e18;

            // Read fee from factory
            (bool factoryOk, bytes memory factoryData) = pools[i].staticcall(abi.encodeWithSignature("factory()"));
            if (factoryOk && factoryData.length >= 32) {
                address factory;
                assembly { factory := mload(add(factoryData, 32)) }
                (bool feeOk, bytes memory feeData) = factory.staticcall(
                    abi.encodeWithSignature("getFee(address,bool)", pools[i], results[i].stable)
                );
                if (feeOk && feeData.length >= 32) {
                    uint256 rawFee;
                    assembly { rawFee := mload(add(feeData, 32)) }
                    results[i].fee = uint32(rawFee);
                }
            }
        }
    }

    function readPcsStable(address[] calldata pools) external view returns (PcsStableState[] memory results) {
        results = new PcsStableState[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];

            (bool c0ok, bytes memory c0d) = pools[i].staticcall(abi.encodeWithSignature("coins(uint256)", 0));
            if (c0ok && c0d.length >= 32) { assembly { mstore(add(results, add(mul(i, 0x120), 0x40)), mload(add(c0d, 32))) } }
            (bool c1ok, bytes memory c1d) = pools[i].staticcall(abi.encodeWithSignature("coins(uint256)", 1));
            if (c1ok && c1d.length >= 32) { assembly { mstore(add(results, add(mul(i, 0x120), 0x60)), mload(add(c1d, 32))) } }

            // Use try/catch for cleaner token reads
            try this._readPcsTokens(pools[i]) returns (address t0, address t1) {
                results[i].token0 = t0;
                results[i].token1 = t1;
            } catch {}

            (bool b0ok, bytes memory b0d) = pools[i].staticcall(abi.encodeWithSignature("balances(uint256)", 0));
            if (b0ok && b0d.length >= 32) {
                uint256 val; assembly { val := mload(add(b0d, 32)) }
                results[i].balance0 = val;
            }
            (bool b1ok, bytes memory b1d) = pools[i].staticcall(abi.encodeWithSignature("balances(uint256)", 1));
            if (b1ok && b1d.length >= 32) {
                uint256 val; assembly { val := mload(add(b1d, 32)) }
                results[i].balance1 = val;
            }

            (bool aOk, bytes memory aData) = pools[i].staticcall(abi.encodeWithSignature("A()"));
            if (aOk && aData.length >= 32) {
                uint256 val; assembly { val := mload(add(aData, 32)) }
                results[i].A = val;
            }

            (bool fOk, bytes memory fData) = pools[i].staticcall(abi.encodeWithSignature("fee()"));
            if (fOk && fData.length >= 32) {
                uint256 val; assembly { val := mload(add(fData, 32)) }
                results[i].fee = val;
            }

            (bool afOk, bytes memory afData) = pools[i].staticcall(abi.encodeWithSignature("admin_fee()"));
            if (afOk && afData.length >= 32) {
                uint256 val; assembly { val := mload(add(afData, 32)) }
                results[i].adminFee = val;
            }
        }
    }

    function _readPcsTokens(address pool) external view returns (address t0, address t1) {
        (bool c0ok, bytes memory c0d) = pool.staticcall(abi.encodeWithSignature("coins(uint256)", 0));
        require(c0ok && c0d.length >= 32, "c0");
        assembly { t0 := mload(add(c0d, 32)) }

        (bool c1ok, bytes memory c1d) = pool.staticcall(abi.encodeWithSignature("coins(uint256)", 1));
        require(c1ok && c1d.length >= 32, "c1");
        assembly { t1 := mload(add(c1d, 32)) }
    }

    function readDodoV2(address[] calldata pools) external view returns (DodoV2State[] memory results) {
        results = new DodoV2State[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            results[i].baseToken = _staticAddr(pools[i], "_BASE_TOKEN_()");
            results[i].quoteToken = _staticAddr(pools[i], "_QUOTE_TOKEN_()");
            results[i].baseReserve = _staticU256(pools[i], "_BASE_RESERVE_()");
            results[i].quoteReserve = _staticU256(pools[i], "_QUOTE_RESERVE_()");
            results[i].baseTarget = _staticU256(pools[i], "_BASE_TARGET_()");
            results[i].quoteTarget = _staticU256(pools[i], "_QUOTE_TARGET_()");
            results[i].k = _staticU256(pools[i], "_K_()");
            results[i].lpFeeRate = _staticU256(pools[i], "_LP_FEE_RATE_()");
            results[i].mtFeeRate = _staticU256(pools[i], "_MT_FEE_RATE_()");

            (bool rOk, bytes memory rData) = pools[i].staticcall(abi.encodeWithSignature("_R_STATE_()"));
            if (rOk && rData.length >= 32) {
                uint256 val; assembly { val := mload(add(rData, 32)) }
                results[i].rState = uint8(val);
            }
        }
    }

    function readWombat(
        address[] calldata pools,
        address[] calldata token0s,
        address[] calldata token1s
    ) external view returns (WombatState[] memory results) {
        require(pools.length == token0s.length && pools.length == token1s.length, "len");
        results = new WombatState[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            results[i].token0 = token0s[i];
            results[i].token1 = token1s[i];

            // Read asset contracts for each token
            address asset0 = _wombatAsset(pools[i], token0s[i]);
            address asset1 = _wombatAsset(pools[i], token1s[i]);

            if (asset0 != address(0)) {
                results[i].cash0 = _staticU256(asset0, "cash()");
                results[i].liability0 = _staticU256(asset0, "liability()");
            }
            if (asset1 != address(0)) {
                results[i].cash1 = _staticU256(asset1, "cash()");
                results[i].liability1 = _staticU256(asset1, "liability()");
            }

            results[i].ampFactor = _staticU256(pools[i], "ampFactor()");
            results[i].haircutRate = _staticU256(pools[i], "haircutRate()");
        }
    }

    // ═══════ Helpers ═══════

    function _staticU256(address target, string memory sig) internal view returns (uint256 result) {
        (bool ok, bytes memory data) = target.staticcall(abi.encodeWithSignature(sig));
        if (ok && data.length >= 32) {
            assembly { result := mload(add(data, 32)) }
        }
    }

    function _staticAddr(address target, string memory sig) internal view returns (address result) {
        (bool ok, bytes memory data) = target.staticcall(abi.encodeWithSignature(sig));
        if (ok && data.length >= 32) {
            assembly { result := mload(add(data, 32)) }
        }
    }

    function _wombatAsset(address pool, address token) internal view returns (address asset) {
        (bool ok, bytes memory data) = pool.staticcall(
            abi.encodeWithSignature("addressOfAsset(address)", token)
        );
        if (ok && data.length >= 32) {
            assembly { asset := mload(add(data, 32)) }
        }
    }
}
