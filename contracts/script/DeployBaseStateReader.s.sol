// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/StateReader.sol";

contract DeployBaseStateReader is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_KEY");
        vm.startBroadcast(deployerKey);

        StateReader reader = new StateReader();
        console.log("StateReader deployed at:", address(reader));

        vm.stopBroadcast();
    }
}
