// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../src/StateReader.sol";

contract StateReaderBscForkTest is Test {
    StateReader reader;

    // From config/bsc.toml
    address constant PCS_USDT_WBNB = 0x16b9a82891338f9bA80E2D6970FddA79D1eb0daE;
    address constant BS_USDT_WBNB = 0x8840C6252e2e86e545deFb6da98B2a0E26d8C1BA;
    address constant PCS_V3_USDT_WBNB = 0x6fe9E9de56356F7eDBfcBB29FAB7cd69471a4869;
    address constant THENA_USDT_WBNB = 0xD405b976Ac01023c9064024880999fC450A8668b;

    function setUp() public {
        uint256 fork = vm.createFork(vm.envString("BSC_RPC_URL"));
        vm.selectFork(fork);
        reader = new StateReader();
    }

    function test_readV2_pcs() public view {
        address[] memory pools = new address[](1);
        pools[0] = PCS_USDT_WBNB;
        StateReader.V2State[] memory r = reader.readV2(pools);

        assertEq(r.length, 1);
        assertEq(r[0].pool, PCS_USDT_WBNB);
        assertTrue(r[0].token0 != address(0), "token0");
        assertTrue(r[0].token1 != address(0), "token1");
        assertTrue(r[0].reserve0 > 0, "reserve0");
        assertTrue(r[0].reserve1 > 0, "reserve1");
    }

    function test_readV2_biswap() public view {
        address[] memory pools = new address[](1);
        pools[0] = BS_USDT_WBNB;
        StateReader.V2State[] memory r = reader.readV2(pools);

        assertEq(r.length, 1);
        assertTrue(r[0].reserve0 > 0, "reserves populated");
    }

    function test_readV3_pcs() public view {
        address[] memory pools = new address[](1);
        pools[0] = PCS_V3_USDT_WBNB;
        StateReader.V3State[] memory r = reader.readV3(pools);

        assertEq(r.length, 1);
        assertTrue(r[0].sqrtPriceX96 > 0, "sqrtPrice");
        assertTrue(r[0].liquidity > 0, "liquidity");
        assertTrue(r[0].fee > 0, "fee");
    }

    function test_readAlgebra_thena() public view {
        address[] memory pools = new address[](1);
        pools[0] = THENA_USDT_WBNB;
        StateReader.AlgebraState[] memory r = reader.readAlgebra(pools);

        assertEq(r.length, 1);
        assertTrue(r[0].sqrtPriceX96 > 0, "sqrtPrice");
        assertTrue(r[0].liquidity > 0, "liquidity");
        assertTrue(r[0].feeZto > 0, "feeZto");
        assertTrue(r[0].feeOtz > 0, "feeOtz");
    }

    function test_readV2_batch() public view {
        address[] memory pools = new address[](2);
        pools[0] = PCS_USDT_WBNB;
        pools[1] = BS_USDT_WBNB;
        StateReader.V2State[] memory r = reader.readV2(pools);
        assertEq(r.length, 2);
        assertTrue(r[0].reserve0 > 0);
        assertTrue(r[1].reserve0 > 0);
    }

    function test_readV2_empty() public view {
        address[] memory pools = new address[](0);
        assertEq(reader.readV2(pools).length, 0);
    }

    function test_readV3_empty() public view {
        address[] memory pools = new address[](0);
        assertEq(reader.readV3(pools).length, 0);
    }
}

contract StateReaderBaseForkTest is Test {
    StateReader reader;

    // From config/base.toml
    address constant AERO_WETH_USDC_VOL = 0xcDAC0d6c6C59727a65F871236188350531885C43;
    address constant UNI_V3_WETH_USDC = 0xd0b53D9277642d899DF5C87A3966A349A798F224;
    address constant SLIP_WETH_USDC = 0xb2cc224c1c9feE385f8ad6a55b4d94E92359DC59;

    function setUp() public {
        uint256 fork = vm.createFork(vm.envString("BASE_RPC_URL"));
        vm.selectFork(fork);
        reader = new StateReader();
    }

    function test_readAeroV2() public view {
        address[] memory pools = new address[](1);
        pools[0] = AERO_WETH_USDC_VOL;
        StateReader.AeroV2State[] memory r = reader.readAeroV2(pools);

        assertEq(r.length, 1);
        assertEq(r[0].pool, AERO_WETH_USDC_VOL);
        assertTrue(r[0].token0 != address(0), "token0");
        assertTrue(r[0].token1 != address(0), "token1");
        assertTrue(r[0].reserve0 > 0, "reserve0");
        assertTrue(r[0].reserve1 > 0, "reserve1");
        assertTrue(r[0].fee > 0, "fee from factory");
    }

    function test_readV3_uni_base() public view {
        address[] memory pools = new address[](1);
        pools[0] = UNI_V3_WETH_USDC;
        StateReader.V3State[] memory r = reader.readV3(pools);

        assertEq(r.length, 1);
        assertTrue(r[0].sqrtPriceX96 > 0, "sqrtPrice");
        assertTrue(r[0].fee > 0, "fee");
        assertTrue(r[0].liquidity > 0, "liquidity");
    }

    function test_readV3_slipstream() public view {
        address[] memory pools = new address[](1);
        pools[0] = SLIP_WETH_USDC;
        StateReader.V3State[] memory r = reader.readV3(pools);

        assertEq(r.length, 1);
        assertTrue(r[0].sqrtPriceX96 > 0, "sqrtPrice");
    }
}
