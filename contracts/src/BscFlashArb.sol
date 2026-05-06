// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title BscFlashArb
 * @notice Cross-DEX arbitrage on BNB Smart Chain targeting PancakeSwap V3 + Uniswap V3/V4
 *         plus PancakeSwap StableSwap, Wombat, DODO V2, and Thena Fusion (Algebra).
 *
 * Flash-loan source: Uniswap V4 PoolManager unlock/take pattern.
 * Swap execution: V3/V4 callback-pull, V2 transfer-then-swap, PCS Stable/Wombat approve-pull,
 *                 DODO V2 transfer-then-call, Algebra callback-pull.
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
    function currencyDelta(address caller, address currency) external view returns (int256);
}

interface IUnlockCallback {
    function unlockCallback(bytes calldata data) external returns (bytes memory);
}

// V3-compatible pool interface (works for both Uniswap V3 and PancakeSwap V3)
interface IV3Pool {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
    function flash(address recipient, uint256 amount0, uint256 amount1, bytes calldata data) external;
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96, bytes calldata data) external returns (int256, int256);
}

// V2-compatible pair interface (works for PancakeSwap V2, Uniswap V2, BiSwap, etc.)
interface IV2Pair {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function swap(uint amount0Out, uint amount1Out, address to, bytes calldata data) external;
    function swapFee() external view returns (uint32);
}

// Uniswap V3 callback interfaces
interface IUniswapV3FlashCallback {
    function uniswapV3FlashCallback(uint256 fee0, uint256 fee1, bytes calldata data) external;
}
interface IUniswapV3SwapCallback {
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

// PancakeSwap V3 callback interfaces (identical signatures, different names)
interface IPancakeV3FlashCallback {
    function pancakeV3FlashCallback(uint256 fee0, uint256 fee1, bytes calldata data) external;
}
interface IPancakeV3SwapCallback {
    function pancakeV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

// ============ New Protocol Interfaces ============

interface IPancakeStable {
    function exchange(uint256 i, uint256 j, uint256 dx, uint256 min_dy) external;
    function coins(uint256) external view returns (address);
    function get_dy(uint256 i, uint256 j, uint256 dx) external view returns (uint256);
    function fee() external view returns (uint256);
}

interface IWombatPool {
    function swap(address fromToken, address toToken, uint256 fromAmount,
                  uint256 minimumToAmount, address to, uint256 deadline)
        external returns (uint256 actualToAmount, uint256 haircut);
    function quotePotentialSwap(address fromToken, address toToken, int256 fromAmount)
        external view returns (uint256 potentialOutcome, uint256 haircut);
}

interface IDODOV2Pool {
    function sellBase(address to) external returns (uint256 receiveQuoteAmount);
    function sellQuote(address to) external returns (uint256 receiveBaseAmount);
    function _BASE_TOKEN_() external view returns (address);
    function _QUOTE_TOKEN_() external view returns (address);
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

contract BscFlashArb is
    IUnlockCallback,
    IUniswapV3FlashCallback,
    IUniswapV3SwapCallback,
    IPancakeV3FlashCallback,
    IPancakeV3SwapCallback,
    IAlgebraSwapCallback
{
    using SafeERC20 for IERC20;

    // ============ Immutables ============

    address public immutable OWNER;
    address public immutable POOL_MANAGER;
    uint256 public immutable CHAIN_ID;

    // ============ State ============

    bool public paused;
    uint256 public maxGasPrice;
    uint256 public minProfitBasisPoints;
    mapping(address => bool) public supportedTokens;

    uint256 public totalExecutions;
    uint256 public totalProfit;

    // Transient storage for callback context
    address private _currentAsset;
    uint256 private _currentAmount;
    uint256 private _gasStart;
    address private _expectedV3SwapPool;

    // Per-pool metadata for protocols that need direction/index info
    // Key: keccak256(abi.encodePacked(pool, tokenIn))
    struct PoolMeta {
        bool registered;
        uint8 indexI;     // PCS Stable: i index; DODO_V2: 0=sellBase, 1=sellQuote
        uint8 indexJ;     // PCS Stable: j index; unused otherwise
        address tokenIn;
        address tokenOut;
    }
    mapping(bytes32 => PoolMeta) public poolMeta;

    // ============ Constants ============

    uint160 internal constant MIN_SQRT_RATIO_PLUS_ONE = 4295128740;
    uint160 internal constant MAX_SQRT_RATIO_MINUS_ONE = 1461446703485210103287273052203988822378723970341;

    // ============ Errors ============

    error Unauthorized();
    error ContractPaused();
    error GasPriceTooHigh(uint256 current, uint256 max);
    error InvalidAmount();
    error UnsupportedToken(address token);
    error SwapFailed(string reason);
    error InsufficientProfit(uint256 actual, uint256 required);
    error UnsettledDelta(address currency, int256 delta);
    error InvalidProtocol(uint8 protocol);

    // ============ Events ============

    event ArbitrageExecuted(address indexed asset, uint256 amount, uint256 profit, uint256 gasUsed, uint8 protocol);
    event TokenSupportUpdated(address indexed token, bool supported);
    event ConfigUpdated(string param, uint256 value);

    // ============ Enums ============

    enum Protocol { V3, V4, V2, PCS_STABLE, WOMBAT, DODO_V2, ALGEBRA }

    // ============ Structs ============

    struct SwapInstruction {
        Protocol protocol;
        address pool;
        PoolKey poolKey;
        address tokenIn;
        address tokenOut;
        uint256 minOut;
    }

    // ============ Modifiers ============

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

    // ============ Constructor ============

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
            revert SwapFailed("Take failed - insufficient tokens received");
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
        } else if (instr.protocol == Protocol.PCS_STABLE) {
            _executePcsStableSwap(instr);
        } else if (instr.protocol == Protocol.WOMBAT) {
            _executeWombatSwap(instr);
        } else if (instr.protocol == Protocol.DODO_V2) {
            _executeDodoV2Swap(instr);
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

        uint160 sqrtPriceLimit = zeroForOne
            ? MIN_SQRT_RATIO_PLUS_ONE
            : MAX_SQRT_RATIO_MINUS_ONE;

        SwapParams memory params = SwapParams({
            zeroForOne: zeroForOne,
            amountSpecified: -int256(amountIn),
            sqrtPriceLimitX96: sqrtPriceLimit
        });

        BalanceDelta delta = IPoolManager(POOL_MANAGER).swap(
            instr.poolKey,
            params,
            ""
        );

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

        if (outputAmount == 0) {
            revert SwapFailed("V4 swap returned no output");
        }
        if (outputAmount < instr.minOut) {
            revert SwapFailed("V4 slippage exceeded");
        }
    }

    // ============ V3 Execution (works for both Uniswap V3 and PancakeSwap V3) ============

    function _executeV3Swap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address poolToken0 = IV3Pool(instr.pool).token0();
        address poolToken1 = IV3Pool(instr.pool).token1();

        bool zeroForOne;
        if (instr.tokenIn == poolToken0 && instr.tokenOut == poolToken1) {
            zeroForOne = true;
        } else if (instr.tokenIn == poolToken1 && instr.tokenOut == poolToken0) {
            zeroForOne = false;
        } else {
            revert SwapFailed("V3 token mismatch");
        }

        uint160 sqrtPriceLimit = zeroForOne
            ? MIN_SQRT_RATIO_PLUS_ONE
            : MAX_SQRT_RATIO_MINUS_ONE;

        bytes memory callbackData = abi.encode(instr.pool, instr.tokenIn);

        _expectedV3SwapPool = instr.pool;

        IV3Pool(instr.pool).swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            sqrtPriceLimit,
            callbackData
        );

        _expectedV3SwapPool = address(0);

        uint256 outputBalance = IERC20(instr.tokenOut).balanceOf(address(this));
        if (outputBalance < instr.minOut) {
            revert SwapFailed("V3 slippage exceeded");
        }
    }

    // ============ V2 Execution (PancakeSwap V2, BiSwap, etc.) ============

    function _executeV2Swap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address token0 = IV2Pair(instr.pool).token0();
        address token1 = IV2Pair(instr.pool).token1();
        (uint112 reserve0, uint112 reserve1, ) = IV2Pair(instr.pool).getReserves();

        bool isToken0In = (instr.tokenIn == token0);
        if (!isToken0In && instr.tokenIn != token1) revert SwapFailed("V2 token mismatch");

        uint256 reserveIn = isToken0In ? uint256(reserve0) : uint256(reserve1);
        uint256 reserveOut = isToken0In ? uint256(reserve1) : uint256(reserve0);

        uint256 amountInWithFee;
        uint256 denominator;
        try IV2Pair(instr.pool).swapFee() returns (uint32 f) {
            amountInWithFee = amountIn * (1000 - uint256(f));
            denominator = reserveIn * 1000 + amountInWithFee;
        } catch {
            amountInWithFee = amountIn * 9975;
            denominator = reserveIn * 10000 + amountInWithFee;
        }

        uint256 amountOut = (amountInWithFee * reserveOut) / denominator;

        if (amountOut == 0) revert SwapFailed("V2 zero output");

        IERC20(instr.tokenIn).safeTransfer(instr.pool, amountIn);

        uint256 amount0Out = isToken0In ? uint256(0) : amountOut;
        uint256 amount1Out = isToken0In ? amountOut : uint256(0);
        IV2Pair(instr.pool).swap(amount0Out, amount1Out, address(this), "");

        if (amountOut < instr.minOut) revert SwapFailed("V2 slippage exceeded");
    }

    // ============ PancakeSwap StableSwap Execution ============

    function _executePcsStableSwap(SwapInstruction memory instr) internal {
        bytes32 key = keccak256(abi.encodePacked(instr.pool, instr.tokenIn));
        PoolMeta memory m = poolMeta[key];
        if (!m.registered) revert SwapFailed("PCS_STABLE not registered");
        if (m.tokenIn != instr.tokenIn || m.tokenOut != instr.tokenOut)
            revert SwapFailed("PCS_STABLE token mismatch");

        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        uint256 balBefore = IERC20(instr.tokenOut).balanceOf(address(this));
        IPancakeStable(instr.pool).exchange(uint256(m.indexI), uint256(m.indexJ), amountIn, instr.minOut);
        uint256 received = IERC20(instr.tokenOut).balanceOf(address(this)) - balBefore;
        if (received == 0) revert SwapFailed("PCS_STABLE zero out");
    }

    // ============ Wombat Exchange Execution ============

    function _executeWombatSwap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        uint256 balBefore = IERC20(instr.tokenOut).balanceOf(address(this));
        IWombatPool(instr.pool).swap(
            instr.tokenIn, instr.tokenOut, amountIn,
            instr.minOut, address(this), block.timestamp + 60
        );
        uint256 received = IERC20(instr.tokenOut).balanceOf(address(this)) - balBefore;
        if (received == 0) revert SwapFailed("WOMBAT zero out");
    }

    // ============ DODO V2 Execution (transfer-then-call, no approve) ============

    function _executeDodoV2Swap(SwapInstruction memory instr) internal {
        bytes32 key = keccak256(abi.encodePacked(instr.pool, instr.tokenIn));
        PoolMeta memory m = poolMeta[key];
        if (!m.registered) revert SwapFailed("DODO not registered");
        if (m.tokenIn != instr.tokenIn || m.tokenOut != instr.tokenOut)
            revert SwapFailed("DODO token mismatch");

        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        IERC20(instr.tokenIn).safeTransfer(instr.pool, amountIn);

        uint256 received;
        if (m.indexI == 0) {
            received = IDODOV2Pool(instr.pool).sellBase(address(this));
        } else {
            received = IDODOV2Pool(instr.pool).sellQuote(address(this));
        }
        if (received < instr.minOut) revert SwapFailed("DODO slippage");
    }

    // ============ Thena Fusion / Algebra Execution ============

    function _executeAlgebraSwap(SwapInstruction memory instr) internal {
        uint256 amountIn = IERC20(instr.tokenIn).balanceOf(address(this));
        if (amountIn == 0) revert InvalidAmount();

        address t0 = IAlgebraPool(instr.pool).token0();
        address t1 = IAlgebraPool(instr.pool).token1();
        bool zeroForOne;
        if (instr.tokenIn == t0 && instr.tokenOut == t1) zeroForOne = true;
        else if (instr.tokenIn == t1 && instr.tokenOut == t0) zeroForOne = false;
        else revert SwapFailed("ALGEBRA token mismatch");

        uint160 sqrtLimit = zeroForOne ? MIN_SQRT_RATIO_PLUS_ONE : MAX_SQRT_RATIO_MINUS_ONE;
        bytes memory cbData = abi.encode(instr.pool, instr.tokenIn);

        _expectedV3SwapPool = instr.pool;
        IAlgebraPool(instr.pool).swap(address(this), zeroForOne, int256(amountIn), sqrtLimit, cbData);
        _expectedV3SwapPool = address(0);

        uint256 outBal = IERC20(instr.tokenOut).balanceOf(address(this));
        if (outBal < instr.minOut) revert SwapFailed("ALGEBRA slippage");
    }

    // ============ Flash Callbacks ============

    function uniswapV3FlashCallback(uint256 fee0, uint256 fee1, bytes calldata data) external override {
        _handleFlashCallback(fee0, fee1, data);
    }

    function pancakeV3FlashCallback(uint256 fee0, uint256 fee1, bytes calldata data) external override {
        _handleFlashCallback(fee0, fee1, data);
    }

    function _handleFlashCallback(uint256 fee0, uint256 fee1, bytes calldata data) internal {
        (
            address flashPool,
            address asset,
            uint256 amount,
            SwapInstruction[] memory swapInstructions,
            uint256 gasStart
        ) = abi.decode(data, (address, address, uint256, SwapInstruction[], uint256));

        if (msg.sender != flashPool) revert Unauthorized();

        address token0 = IV3Pool(flashPool).token0();
        uint256 fee = asset == token0 ? fee0 : fee1;
        uint256 amountOwed = amount + fee;

        for (uint256 i = 0; i < swapInstructions.length; i++) {
            _dispatchSwap(swapInstructions[i]);
        }

        uint256 balanceAfter = IERC20(asset).balanceOf(address(this));
        if (balanceAfter < amountOwed) {
            revert InsufficientProfit(0, minProfitBasisPoints);
        }

        uint256 grossProfit = balanceAfter - amountOwed;
        uint256 requiredProfit = (amount * minProfitBasisPoints) / 10000;

        if (grossProfit < requiredProfit) {
            revert InsufficientProfit(grossProfit, requiredProfit);
        }

        IERC20(asset).safeTransfer(flashPool, amountOwed);

        totalExecutions++;
        totalProfit += grossProfit;

        uint256 gasUsed = gasStart - gasleft();
        emit ArbitrageExecuted(asset, amount, grossProfit, gasUsed, uint8(Protocol.V3));
    }

    // ============ Swap Callbacks ============

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external override {
        _handleSwapCallback(amount0Delta, amount1Delta, data);
    }

    function pancakeV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external override {
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
            revert SwapFailed("Invalid swap callback deltas");
        }

        IERC20(tokenIn).safeTransfer(msg.sender, amountToPay);
    }

    // ============ Admin Functions ============

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

    function pause() external onlyOwner {
        paused = true;
        emit ConfigUpdated("paused", 1);
    }

    function unpause() external onlyOwner {
        paused = false;
        emit ConfigUpdated("paused", 0);
    }

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

    // ============ Emergency Functions ============

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

    // ============ View Functions ============

    function getStats() external view returns (
        uint256 executions,
        uint256 profit,
        bool isPaused,
        uint256 gasLimit,
        uint256 minProfit
    ) {
        return (totalExecutions, totalProfit, paused, maxGasPrice, minProfitBasisPoints);
    }

    receive() external payable {}
}
