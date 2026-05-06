// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/BaseFlashArb.sol";
import "../src/StateReader.sol";

contract DeployBase is Script {
    // Base mainnet Uniswap V4 PoolManager
    address constant POOL_MANAGER = 0x498581fF718922c3f8e6A244956aF099B2652b2b;

    // Base mainnet tokens
    address constant WETH   = 0x4200000000000000000000000000000000000006;
    address constant USDbC  = 0xd9AAec86b65D86f6a7b5B1b0c42fFA531710b6Aa;
    address constant USDC   = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    address constant DAI    = 0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb;
    address constant cbETH  = 0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22;

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerKey);

        address[] memory tokens = new address[](5);
        tokens[0] = WETH;
        tokens[1] = USDbC;
        tokens[2] = USDC;
        tokens[3] = DAI;
        tokens[4] = cbETH;

        BaseFlashArb arb = new BaseFlashArb(
            POOL_MANAGER,
            50 gwei,    // maxGasPrice — Base L2 gas is cheap
            0,          // minProfitBps — accept any profit initially
            tokens
        );

        StateReader reader = new StateReader();

        console.log("BaseFlashArb deployed at:", address(arb));
        console.log("StateReader deployed at:", address(reader));
        console.log("Owner:", arb.OWNER());

        vm.stopBroadcast();
    }
}
