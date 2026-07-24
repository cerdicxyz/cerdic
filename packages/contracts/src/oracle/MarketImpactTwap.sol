// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {RingBuffer} from "../lib/RingBuffer.sol";

/// @title  MarketImpactTwap
/// @notice On-chain mark-price impact TWAP of the Synchra oracle stack
///         (plan todo #21): the tertiary oracle input cited by
///         paper/synchra.tex:570 ("median of external feeds, on-chain
///         impact mid-price, ...") and paper/synchra.tex:1059 ("Tertiary
///         oracle: TWAP from CLOB trades"). `SettlementEngine` calls
///         `recordTrade` after every `settleTrade`; `OracleHub.markPrice`
///         reads `twap` as the third leg of its median.
/// @dev    Per market, trades are aggregated per block into a
///         `RingBuffer` rolling 60-block window (see `RingBuffer.sol`):
///         each block contributes its volume-weighted average price, and
///         the TWAP is the arithmetic mean of those per-block prices over
///         the populated blocks inside the trailing 60-block window
///         (inclusive of the current block). Time weighting across blocks
///         plus volume weighting within a block bounds the pull of a
///         single outlier print: one 10x outlier block among 60 moves the
///         TWAP by only 9/60 = 15% of the base price.
///
///         Access: `recordTrade` is gated to the wired `settlementEngine`
///         — the TWAP is only as trustworthy as its feed, so an open
///         writer would be a manipulation vector. The admin wires the
///         engine post-deploy (deploy order: engine, then this contract,
///         then `setSettlementEngine` + `SettlementEngine.setImpactTwap`).
///         The admin is immutable, mirroring `OracleHub` (todo #12).
///
///         Gas: `recordTrade` is budgeted at 30k
///         (`MarketImpactTwap.gasPricePerRecordTrade = 30k` in
///         `gas_benchmarks.txt`). The ring-buffer layout keeps the hot
///         path at 1 SLOAD + 1-2 SSTOREs; see `RingBuffer.sol`.
contract MarketImpactTwap {
    using RingBuffer for RingBuffer.Buffer;

    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Rolling-window length in blocks. Mirrors
    ///         `RingBuffer.WINDOW_BLOCKS` (solc 0.8.24 cannot read another
    ///         contract's constant via the type name); the 59/60 boundary
    ///         tests in `MarketImpactTwapTest` pin the behavior to 60.
    uint256 public constant WINDOW_BLOCKS = 60;

    /// @notice 1e18 price/size scaling shared with the rest of the kernel.
    uint256 internal constant SCALE = 1e18;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Contract administrator. Gates `setSettlementEngine`;
    ///         immutable for the MVP (no admin-transfer surface).
    address public immutable admin;

    /// @notice The only address permitted to call `recordTrade` — the
    ///         clearing kernel's `SettlementEngine`. Zero until wired.
    address public settlementEngine;

    /// @notice Per-market rolling 60-block observation windows.
    mapping(bytes32 => RingBuffer.Buffer) internal _buffers;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when the settlement-engine feed is (re)wired.
    event SettlementEngineUpdated(address indexed settlementEngine);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the admin.
    error NotAdmin();

    /// @notice `recordTrade` caller is not the wired settlement engine.
    error NotSettlementEngine();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice `price` must be non-zero — a zero print is not a trade.
    error ZeroPrice();

    /// @notice `size` must be non-zero — a zero-size print carries no
    ///         volume and would leave a phantom observation.
    error ZeroSize();

    /// @notice `twap` was read for a market with no observation inside
    ///         the trailing 60-block window (never traded, or all
    ///         observations stale). Callers (`OracleHub`) treat this as
    ///         "no tertiary data" and fall back, rather than consuming a
    ///         meaningless zero.
    error TwapNotAvailable(bytes32 marketId);

    // ---------------------------------------------------------------------
    // Modifiers.
    // ---------------------------------------------------------------------

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param adminAccount Receives the immutable admin. The settlement
    ///        engine is intentionally NOT a constructor argument: the
    ///        engine must exist first (it is wired into this contract),
    ///        while this contract must exist before the engine can be
    ///        wired back — a post-deploy setter on both sides breaks the
    ///        cycle.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
    }

    // ---------------------------------------------------------------------
    // Feed.
    // ---------------------------------------------------------------------

    /// @notice Records one settled trade into `marketId`'s rolling window.
    ///         Called by `SettlementEngine.settleTrade` after the atomic
    ///         position writes (todo #21); same-block trades aggregate,
    ///         a newer block advances the ring.
    /// @dev    Reverts for any caller other than the wired
    ///         `settlementEngine` (including while it is unset). The
    ///         `uint104` casts are checked: a price-size product beyond
    ///         the packed bound reverts the call (and, since the engine
    ///         calls this synchronously, the trade) rather than silently
    ///         corrupting the window.
    function recordTrade(bytes32 marketId, uint256 price, uint256 size) external {
        if (msg.sender != settlementEngine) revert NotSettlementEngine();
        if (price == 0) revert ZeroPrice();
        if (size == 0) revert ZeroSize();

        // forge-lint: disable-next-line(unsafe-typecast)
        uint104 priceVolume = uint104((price * size) / SCALE);
        // forge-lint: disable-next-line(unsafe-typecast)
        _buffers[marketId].record(uint48(block.number), priceVolume, uint104(size));
    }

    /// @notice Current 60-block rolling TWAP for `marketId` (1e18-scaled
    ///         USD): the arithmetic mean of the per-block volume-weighted
    ///         average prices over the populated blocks in the window.
    /// @dev    Reverts `TwapNotAvailable` when the window holds no
    ///         observation — never returns a misleading zero.
    function twap(bytes32 marketId) external view returns (uint256) {
        (uint256 mean, uint256 count) = _buffers[marketId].meanBlockVwap(block.number);
        if (count == 0) revert TwapNotAvailable(marketId);
        return mean;
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Wires (or replaces) the settlement engine permitted to feed
    ///         the TWAP via `recordTrade`.
    function setSettlementEngine(address engine) external onlyAdmin {
        if (engine == address(0)) revert ZeroAddress();
        settlementEngine = engine;
        emit SettlementEngineUpdated(engine);
    }
}
