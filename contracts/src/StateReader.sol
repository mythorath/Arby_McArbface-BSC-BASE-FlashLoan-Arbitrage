// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title StateReader
 * @notice Batched view-only contract for reading pool state in a single eth_call.
 *         Returns packed data for V2, V3, Aerodrome, and Algebra pools so the Rust
 *         scanner can refresh all pool state in one round trip.
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
        }
    }

    function readV3(address[] calldata pools) external view returns (V3State[] memory results) {
        results = new V3State[](pools.length);
        for (uint256 i = 0; i < pools.length; i++) {
            results[i].pool = pools[i];
            try IV3Like(pools[i]).token0() returns (address t0) { results[i].token0 = t0; } catch {}
            try IV3Like(pools[i]).token1() returns (address t1) { results[i].token1 = t1; } catch {}
            try IV3Like(pools[i]).fee() returns (uint24 f) { results[i].fee = f; } catch {}
            // Low-level call for slot0: PCS V3 has uint32 feeProtocol vs Uniswap V3's uint8.
            // Typed try/catch panics on the ABI mismatch so we decode manually.
            (bool ok, bytes memory data) = pools[i].staticcall(abi.encodeWithSignature("slot0()"));
            if (ok && data.length >= 64) {
                // First word: sqrtPriceX96 (uint160, right-aligned in 32 bytes)
                uint256 word0;
                uint256 word1;
                assembly {
                    word0 := mload(add(data, 32))
                    word1 := mload(add(data, 64))
                }
                results[i].sqrtPriceX96 = uint160(word0);
                results[i].tick = int24(int256(word1));
                results[i].unlocked = true;
                // Check unlocked flag if enough data (last word, first byte)
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
            // Low-level call for globalState — Algebra/Thena versions differ in return types.
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
        }
    }

    function readAll(
        address[] calldata v2Pools,
        address[] calldata v3Pools,
        address[] calldata algebraPools,
        address[] calldata aeroV2Pools
    ) external view returns (
        V2State[] memory v2Results,
        V3State[] memory v3Results,
        AlgebraState[] memory algebraResults,
        AeroV2State[] memory aeroV2Results
    ) {
        v2Results = this.readV2(v2Pools);
        v3Results = this.readV3(v3Pools);
        algebraResults = this.readAlgebra(algebraPools);
        aeroV2Results = this.readAeroV2(aeroV2Pools);
    }
}
