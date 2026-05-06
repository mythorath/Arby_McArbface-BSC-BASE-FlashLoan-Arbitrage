// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title BaseFlashArb
 * @notice Cross-DEX arbitrage on Base targeting Aerodrome (volatile + stable + Slipstream),
 *         Uniswap V2/V3/V4, SushiSwap V3, BaseSwap V2, and Algebra-style CLMMs.
 *
 * Flash-loan source: Uniswap V4 PoolManager unlock/take pattern.
 * Aerodrome integration: Solidly-fork V2 (volatile & stable) via direct pool calls,
 *                        plus Slipstream (CL) via Algebra-compatible callback.
 */

import {IERC20} from "lib/openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "lib/openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";

// ============ V4 Interfaces ============

struct PoolKey {
    address currency0;
    address currency1;
    uint24 fee;
    int24 tickSpacing;
    address hooks;
}

type BalanceDelta is int256;

struct SwapParams {
    bool zeroForOne;
    int256 amountSpecified;
    uint160 sqrtPriceLimitX96;
}

interface IPoolManager {
    function unlock(bytes calldata data) external returns (bytes memory);
    function swap(PoolKey calldata key, SwapParams calldata params, bytes calldata hookData) external returns (BalanceDelta delta);
    function settle() external payable returns (uint256);
    function take(address currency, address to, uint256 amount) external;
    function sync(address currency) external;
}

interface IUnlockCallback {
    function unlockCallback(bytes calldata data) external returns (bytes memory);
}

interface IV3Pool {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96, bytes calldata data) external returns (int256, int256);
}

interface IV2Pair {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function swap(uint amount0Out, uint amount1Out, address to, bytes calldata data) external;
}

interface IUniswapV3SwapCallback {
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

// ============ Aerodrome Interfaces ============

interface IAerodromePool {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function stable() external view returns (bool);
    function getReserves() external view returns (uint256 reserve0, uint256 reserve1, uint256 blockTimestampLast);
    function getAmountOut(uint256 amountIn, address tokenIn) external view returns (uint256);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IAerodromeSlipstream {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96, bytes calldata data) external returns (int256 amount0, int256 amount1);
}

interface IAerodromeSlipstreamCallback {
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

interface IAlgebraPool {
    function swap(address recipient, bool zeroToOne, int256 amountSpecified,
                  uint160 limitSqrtPrice, bytes calldata data)
        external returns (int256 amount0, int256 amount1);
    function token0() external view returns (address);
    function token1() external view returns (address);
}

interface IAlgebraSwapCallback {
    function algebraSwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

// ============ Main Contract ============

contract BaseFlashArb is
    IUnlockCallback,
    IUniswapV3SwapCallback,
    IAlgebraSwapCallback
{
    using SafeERC20 for IERC20;

    address public immutable OWNER;
    address public immutable POOL_MANAGER;
    uint256 public immutable CHAIN_ID;

    bool public paused;
    uint256 public maxGasPrice;
    uint256 public minProfitBasisPoints;
    mapping(address => bool) public supportedTokens;

    uint256 public totalExecutions;
    uint256 public totalProfit;

    address private _currentAsset;
    uint256 private _currentAmount;
    uint256 private _gasStart;
    address private _expectedV3SwapPool;

    struct PoolMeta {
        bool registered;
        uint8 indexI;
        uint8 indexJ;
        address tokenIn;
        address tokenOut;
    }
    mapping(bytes32 => PoolMeta) public poolMeta;

    uint160 internal constant MIN_SQRT_RATIO_PLUS_ONE = 4295128740;
    uint160 internal constant MAX_SQRT_RATIO_MINUS_ONE = 1461446703485210103287273052203988822378723970341;

    error Unauthorized();
    error ContractPaused();
    error GasPriceTooHigh(uint256 current, uint256 max);
    error InvalidAmount();
    error UnsupportedToken(address token);
    error SwapFailed(string reason);
    error InsufficientProfit(uint256 actual, uint256 required);
    error InvalidProtocol(uint8 protocol);

    event ArbitrageExecuted(address indexed asset, uint256 amount, uint256 profit, uint256 gasUsed, uint8 protocol);
    event TokenSupportUpdated(address indexed token, bool supported);
    event ConfigUpdated(string param, uint256 value);

    enum Protocol { V3, V4, V2, AERO_V2, AERO_SLIPSTREAM, ALGEBRA }

    struct SwapInstruction {
        Protocol protocol;
        address pool;
        PoolKey poolKey;
        address tokenIn;
        address tokenOut;
        uint256 minOut;
    }

    modifier onlyOwner() {
        if (msg.sender != OWNER) revert Unauthorized();
        _;
    }

    modifier whenNotPaused() {
        if (paused) revert ContractPaused();
        _;
    }

    modifier checkGasPrice() {
        if (tx.gasprice > maxGasPrice) revert GasPriceTooHigh(tx.gasprice, maxGasPrice);
        _;
    }

    constructor(
        address _poolManager,
        uint256 _maxGasPrice,
        uint256 _minProfitBps,
        address[] memory _supportedTokens
    ) {
        require(_maxGasPrice > 0, "Invalid max gas price");
        OWNER = msg.sender;
        POOL_MANAGER = _poolManager;
        CHAIN_ID = block.chainid;
        maxGasPrice = _maxGasPrice;
        minProfitBasisPoints = _minProfitBps;
        for (uint256 i = 0; i < _supportedTokens.length; i++) {
            supportedTokens[_supportedTokens[i]] = true;
            emit TokenSupportUpdated(_supportedTokens[i], true);
        }
    }

    // ============ V4 Execution (unlock pattern) ============

    function executeV4Arbitrage(
        address asset,
        uint256 amount,
        SwapInstruction[] calldata swapInstructions,
        uint256 deadline
    ) external onlyOwner whenNotPaused checkGasPrice {
        if (block.timestamp > deadline) revert SwapFailed("Transaction expired");
        if (!supportedTokens[asset]) revert UnsupportedToken(asset);
        if (amount == 0) revert InvalidAmount();
        if (swapInstructions.length == 0) revert SwapFailed("No swaps");

        _currentAsset = asset;
        _currentAmount = amount;
        _gasStart = gasleft();

        bytes memory callbackData = abi.encode(asset, amount, swapInstructions);
        IPoolManager(POOL_MANAGER).unlock(callbackData);

        _currentAsset = address(0);
        _currentAmount = 0;
        _gasStart = 0;
    }

    function unlockCallback(bytes calldata data) external override returns (bytes memory) {
        if (msg.sender != POOL_MANAGER) revert Unauthorized();

        (
            address asset,
            uint256 amount,
            SwapInstruction[] memory swapInstructions
        ) = abi.decode(data, (address, uint256, SwapInstruction[]));

        uint256 gasStart = _gasStart;
        bool isNativeETH = asset == address(0);

        uint256 balanceBefore;
        if (isNativeETH) {
            balanceBefore = address(this).balance;
        } else {
            balanceBefore = IERC20(asset).balanceOf(address(this));
        }

        IPoolManager(POOL_MANAGER).take(asset, address(this), amount);

        uint256 balanceAfterTake;
        if (isNativeETH) {
            balanceAfterTake = address(this).balance;
        } else {
            balanceAfterTake = IERC20(asset).balanceOf(address(this));
        }
        if (balanceAfterTake < balanceBefore + amount) {
            revert SwapFailed("Take failed");
        }

        for (uint256 i = 0; i < swapInstructions.length; i++) {
            _dispatchSwap(swapInstructions[i]);
        }

        uint256 balanceAfter;
        if (isNativeETH) {
            balanceAfter = address(this).balance;
        } else {
            balanceAfter = IERC20(asset).balanceOf(address(this));
        }

        if (balanceAfter <= amount) {
            revert InsufficientProfit(0, minProfitBasisPoints);
        }

        uint256 grossProfit = balanceAfter - amount;
        uint256 requiredProfit = (amount * minProfitBasisPoints) / 10000;

        if (grossProfit < requiredProfit) {
            revert InsufficientProfit(grossProfit, requiredProfit);
        }

        if (isNativeETH) {
            IPoolManager(POOL_MANAGER).settle{value: amount}();
        } else {
            IPoolManager(POOL_MANAGER).sync(asset);
            IERC20(asset).safeTransfer(POOL_MANAGER, amount);
            IPoolManager(POOL_MANAGER).settle();
        }

        totalExecutions++;
        totalProfit += grossProfit;

        uint256 gasUsed = gasStart - gasleft();
        emit ArbitrageExecuted(asset, amount, grossProfit, gasUsed, uint8(Protocol.V4));

        return "";
    }

    // ============ Swap Dispatcher ============

    function _dispatchSwap(SwapInstruction memory instr) internal {
        if (instr.protocol == Protocol.V3) {
            _executeV3Swap(instr);
        } else if (instr.protocol == Protocol.V4) {
            _executeV4Swap(instr);
        } else if (instr.protocol == Protocol.V2) {
            _executeV2Swap(instr);
        } else if (instr.protocol == Protocol.AERO_V2) {
            _executeAeroV2Swap(instr);
        } else if (instr.protocol == Protocol.AERO_SLIPSTREAM) {
            _executeAeroSlipstreamSwap(instr);
        } else if (instr.protocol == Protocol.ALGEBRA) {
            _executeAlgebraSwap(instr);
        } else {
            revert InvalidProtocol(uint8(instr.protocol));
        }
    }

    // ============ V4 Swap ============

    function _executeV4Swap(SwapInstruction memory instr) internal {
        bool isNativeETH = instr.tokenIn == address(0);

        uint256 amountIn;
        if (isNativeETH) {
            amountIn = address(this).balance;
        } else {
            amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        }
        if (amountIn == 0) revert InvalidAmount();

        bool zeroForOne = instr.tokenIn == instr.poolKey.currency0;
        uint160 sqrtPriceLimit = zeroForOne ? MIN_SQRT_RATIO_PLUS_ONE : MAX_SQRT_RATIO_MINUS_ONE;

        SwapParams memory params = SwapParams({
            zeroForOne: zeroForOne,
            amountSpecified: -int256(amountIn),
            sqrtPriceLimitX96: sqrtPriceLimit
        });

        BalanceDelta delta = IPoolManager(POOL_MANAGER).swap(instr.poolKey, params, "");

        int256 amount0Delta = int128(int256(BalanceDelta.unwrap(delta) >> 128));
        int256 amount1Delta = int128(int256(BalanceDelta.unwrap(delta)));

        if (amount0Delta < 0) {
            uint256 amountOwed = uint256(-amount0Delta);
            address currency = instr.poolKey.currency0;
            if (currency == address(0)) {
                IPoolManager(POOL_MANAGER).settle{value: amountOwed}();
            } else {
                IPoolManager(POOL_MANAGER).sync(currency);
                IERC20(currency).safeTransfer(POOL_MANAGER, amountOwed);
                IPoolManager(POOL_MANAGER).settle();
            }
        }
        if (amount1Delta < 0) {
            uint256 amountOwed = uint256(-amount1Delta);
            address currency = instr.poolKey.currency1;
            if (currency == address(0)) {
                IPoolManager(POOL_MANAGER).settle{value: amountOwed}();
            } else {
                IPoolManager(POOL_MANAGER).sync(currency);
                IERC20(currency).safeTransfer(POOL_MANAGER, amountOwed);
                IPoolManager(POOL_MANAGER).settle();
            }
        }

        uint256 outputAmount = 0;
        if (amount0Delta > 0) {
            outputAmount = uint256(amount0Delta);
            IPoolManager(POOL_MANAGER).take(instr.poolKey.currency0, address(this), outputAmount);
        }
        if (amount1Delta > 0) {
            outputAmount = uint256(amount1Delta);
            IPoolManager(POOL_MANAGER).take(instr.poolKey.currency1, address(this), outputAmount);
        }

        if (outputAmount == 0) revert SwapFailed("V4 zero output");
        if (outputAmount < instr.minOut) revert SwapFailed("V4 slippage");
    }

    // ============ V3 Swap ============

    function _executeV3Swap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address poolToken0 = IV3Pool(instr.pool).token0();
        bool zeroForOne = instr.tokenIn == poolToken0;

        uint160 sqrtPriceLimit = zeroForOne ? MIN_SQRT_RATIO_PLUS_ONE : MAX_SQRT_RATIO_MINUS_ONE;
        bytes memory callbackData = abi.encode(instr.pool, instr.tokenIn);

        _expectedV3SwapPool = instr.pool;

        IV3Pool(instr.pool).swap(
            address(this), zeroForOne, int256(amountIn), sqrtPriceLimit, callbackData
        );

        _expectedV3SwapPool = address(0);

        uint256 outputBalance = IERC20(instr.tokenOut).balanceOf(address(this));
        if (outputBalance < instr.minOut) revert SwapFailed("V3 slippage");
    }

    // ============ V2 Swap (Uniswap V2, BaseSwap, SushiSwap V2, etc.) ============

    function _executeV2Swap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address token0 = IV2Pair(instr.pool).token0();
        (uint112 reserve0, uint112 reserve1, ) = IV2Pair(instr.pool).getReserves();

        bool isToken0In = (instr.tokenIn == token0);
        uint256 reserveIn = isToken0In ? uint256(reserve0) : uint256(reserve1);
        uint256 reserveOut = isToken0In ? uint256(reserve1) : uint256(reserve0);

        uint256 amountInWithFee = amountIn * 997;
        uint256 amountOut = (amountInWithFee * reserveOut) / (reserveIn * 1000 + amountInWithFee);

        if (amountOut == 0) revert SwapFailed("V2 zero output");

        IERC20(instr.tokenIn).safeTransfer(instr.pool, amountIn);

        uint256 amount0Out = isToken0In ? uint256(0) : amountOut;
        uint256 amount1Out = isToken0In ? amountOut : uint256(0);
        IV2Pair(instr.pool).swap(amount0Out, amount1Out, address(this), "");

        if (amountOut < instr.minOut) revert SwapFailed("V2 slippage");
    }

    // ============ Aerodrome V2 (Solidly-fork volatile + stable) ============

    function _executeAeroV2Swap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        uint256 amountOut = IAerodromePool(instr.pool).getAmountOut(amountIn, instr.tokenIn);
        if (amountOut == 0) revert SwapFailed("AERO_V2 zero output");

        IERC20(instr.tokenIn).safeTransfer(instr.pool, amountIn);

        address token0 = IAerodromePool(instr.pool).token0();
        bool isToken0In = (instr.tokenIn == token0);
        uint256 amount0Out = isToken0In ? uint256(0) : amountOut;
        uint256 amount1Out = isToken0In ? amountOut : uint256(0);
        IAerodromePool(instr.pool).swap(amount0Out, amount1Out, address(this), "");

        if (amountOut < instr.minOut) revert SwapFailed("AERO_V2 slippage");
    }

    // ============ Aerodrome Slipstream (concentrated liquidity) ============

    function _executeAeroSlipstreamSwap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address t0 = IAerodromeSlipstream(instr.pool).token0();
        bool zeroForOne = (instr.tokenIn == t0);

        uint160 sqrtLimit = zeroForOne ? MIN_SQRT_RATIO_PLUS_ONE : MAX_SQRT_RATIO_MINUS_ONE;
        bytes memory cbData = abi.encode(instr.pool, instr.tokenIn);

        _expectedV3SwapPool = instr.pool;
        IAerodromeSlipstream(instr.pool).swap(
            address(this), zeroForOne, int256(amountIn), sqrtLimit, cbData
        );
        _expectedV3SwapPool = address(0);

        uint256 outBal = IERC20(instr.tokenOut).balanceOf(address(this));
        if (outBal < instr.minOut) revert SwapFailed("SLIPSTREAM slippage");
    }

    // ============ Algebra Swap ============

    function _executeAlgebraSwap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address t0 = IAlgebraPool(instr.pool).token0();
        bool zeroForOne = (instr.tokenIn == t0);

        uint160 sqrtLimit = zeroForOne ? MIN_SQRT_RATIO_PLUS_ONE : MAX_SQRT_RATIO_MINUS_ONE;
        bytes memory cbData = abi.encode(instr.pool, instr.tokenIn);

        _expectedV3SwapPool = instr.pool;
        IAlgebraPool(instr.pool).swap(address(this), zeroForOne, int256(amountIn), sqrtLimit, cbData);
        _expectedV3SwapPool = address(0);

        uint256 outBal = IERC20(instr.tokenOut).balanceOf(address(this));
        if (outBal < instr.minOut) revert SwapFailed("ALGEBRA slippage");
    }

    // ============ Swap Callbacks ============

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external override {
        _handleSwapCallback(amount0Delta, amount1Delta, data);
    }

    function algebraSwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external override {
        _handleSwapCallback(amount0Delta, amount1Delta, data);
    }

    function _handleSwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) internal {
        if (msg.sender != _expectedV3SwapPool || _expectedV3SwapPool == address(0)) {
            revert Unauthorized();
        }

        (, address tokenIn) = abi.decode(data, (address, address));

        uint256 amountToPay;
        if (amount0Delta > 0) {
            amountToPay = uint256(amount0Delta);
        } else if (amount1Delta > 0) {
            amountToPay = uint256(amount1Delta);
        } else {
            revert SwapFailed("Invalid callback deltas");
        }

        IERC20(tokenIn).safeTransfer(msg.sender, amountToPay);
    }

    // ============ Admin ============

    function setPoolMeta(
        address pool, address tokenIn, address tokenOut,
        uint8 i, uint8 j
    ) external onlyOwner {
        bytes32 key = keccak256(abi.encodePacked(pool, tokenIn));
        poolMeta[key] = PoolMeta({
            registered: true,
            indexI: i,
            indexJ: j,
            tokenIn: tokenIn,
            tokenOut: tokenOut
        });
    }

    function approveToken(address token, address spender, uint256 amount) external onlyOwner {
        IERC20(token).forceApprove(spender, amount);
    }

    function pause() external onlyOwner { paused = true; emit ConfigUpdated("paused", 1); }
    function unpause() external onlyOwner { paused = false; emit ConfigUpdated("paused", 0); }

    function setMaxGasPrice(uint256 _maxGasPrice) external onlyOwner {
        require(_maxGasPrice > 0, "Invalid value");
        maxGasPrice = _maxGasPrice;
        emit ConfigUpdated("maxGasPrice", _maxGasPrice);
    }

    function setMinProfitBasisPoints(uint256 _minProfitBps) external onlyOwner {
        require(_minProfitBps <= 1000, "Max 10%");
        minProfitBasisPoints = _minProfitBps;
        emit ConfigUpdated("minProfitBasisPoints", _minProfitBps);
    }

    function setTokenSupport(address token, bool supported) external onlyOwner {
        supportedTokens[token] = supported;
        emit TokenSupportUpdated(token, supported);
    }

    function addTokens(address[] calldata tokens) external onlyOwner {
        for (uint256 i = 0; i < tokens.length; i++) {
            supportedTokens[tokens[i]] = true;
            emit TokenSupportUpdated(tokens[i], true);
        }
    }

    function emergencyWithdraw(address token, address to, uint256 amount) external onlyOwner {
        IERC20(token).safeTransfer(to, amount);
    }

    function emergencyWithdrawETH(address payable to) external onlyOwner {
        uint256 balance = address(this).balance;
        if (balance > 0) {
            (bool success, ) = to.call{value: balance}("");
            require(success, "ETH transfer failed");
        }
    }

    function getStats() external view returns (
        uint256 executions, uint256 profit, bool isPaused,
        uint256 gasLimit, uint256 minProfit
    ) {
        return (totalExecutions, totalProfit, paused, maxGasPrice, minProfitBasisPoints);
    }

    receive() external payable {}
}
