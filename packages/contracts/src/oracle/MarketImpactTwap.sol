// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {RingBuffer} from "../lib/RingBuffer.sol";

/// @title  MarketImpactTwap
/// @notice On-chain tertiary oracle leg: SettlementEngine calls recordTrade after every
///         settleTrade, OracleHub.markPrice reads twap() as the third median input.
/// @dev    Per-market RingBuffer (see RingBuffer.sol): time weighting across blocks plus
///         volume weighting within a block bounds one outlier's pull (a 10x outlier block
///         among 60 moves the TWAP by only 9/60 = 15%). recordTrade is gated to the wired
///         settlementEngine only — an open writer would be a manipulation vector.
contract MarketImpactTwap {
    using RingBuffer for RingBuffer.Buffer;

    /// @notice Mirrors RingBuffer.WINDOW_BLOCKS; pinned by MarketImpactTwapTest.
    uint256 public constant WINDOW_BLOCKS = 60;
    uint256 internal constant SCALE = 1e18;

    address public immutable admin;

    /// @notice Zero until wired; only address permitted to call recordTrade.
    address public settlementEngine;

    mapping(bytes32 => RingBuffer.Buffer) internal _buffers;

    event SettlementEngineUpdated(address indexed settlementEngine);

    error NotAdmin();
    error NotSettlementEngine();
    error ZeroAddress();
    error ZeroPrice();
    error ZeroSize();

    /// @notice No observation inside the trailing window; callers (OracleHub) fall back
    ///         rather than consuming a meaningless zero.
    error TwapNotAvailable(bytes32 marketId);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    /// @dev settlementEngine isn't a constructor arg: the engine must exist before it can
    ///      be wired here, and this contract must exist before the engine wires back to it.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
    }

    /// @dev The uint104 casts are checked: an out-of-range price*size reverts the trade
    ///      rather than silently corrupting the window.
    function recordTrade(bytes32 marketId, uint256 price, uint256 size) external {
        if (msg.sender != settlementEngine) revert NotSettlementEngine();
        if (price == 0) revert ZeroPrice();
        if (size == 0) revert ZeroSize();

        // forge-lint: disable-next-line(unsafe-typecast)
        uint104 priceVolume = uint104((price * size) / SCALE);
        // forge-lint: disable-next-line(unsafe-typecast)
        _buffers[marketId].record(uint48(block.number), priceVolume, uint104(size));
    }

    function twap(bytes32 marketId) external view returns (uint256) {
        (uint256 mean, uint256 count) = _buffers[marketId].meanBlockVwap(block.number);
        if (count == 0) revert TwapNotAvailable(marketId);
        return mean;
    }

    function setSettlementEngine(address engine) external onlyAdmin {
        if (engine == address(0)) revert ZeroAddress();
        settlementEngine = engine;
        emit SettlementEngineUpdated(engine);
    }
}
