// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";

/// @title IPositionDecoder
/// @notice Per-market decoder surface the position engine uses to read typed
///         metadata out of a position's opaque bytes
///         (paper/synchra.tex:396-411).
/// @dev    Implemented by each market extension — the perp extension
///         (`BtcPerpMarket.sol`, todo #14) decodes its own encoded
///         `MarketPosition` struct. The kernel stays market-agnostic: it
///         never interprets position bytes itself, it only forwards them to
///         the decoder registered for that market.
///
///         `IMarket` (todo #11) folds this surface into the market
///         extension's canonical interface as `getMetadata`; the two are
///         kept ABI-compatible so the same contract serves both.
interface IPositionDecoder {
    /// @notice Decodes a position's opaque bytes into the canonical
    ///         metadata quadruple the risk engine consumes.
    /// @param  positionData Opaque position bytes as stored by the engine.
    /// @return size       Position size (base units, 1e18-scaled).
    /// @return entryPrice Volume-weighted entry price (1e18-scaled USD).
    /// @return margin     Margin locked against the position (1e18-scaled).
    /// @return leverage   Per-market leverage ceiling, not effective
    ///                    leverage (paper/synchra.tex:410-411).
    function getMetadata(bytes calldata positionData)
        external
        view
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage);
}

/// @title  PositionEngine
/// @notice Position engine of the Synchra clearing kernel
///         (paper/synchra.tex:388-411). Maintains the position set
///         `P = {p_m | m in M}` per account (paper line 357-360), storing
///         each position as opaque bytes (paper line 409) so the kernel
///         stays market-agnostic while market extensions own their own
///         encoding.
/// @dev    MVP scope guardrails:
///         - No funding accrual — the lazy funding-index model
///           (paper/synchra.tex:419-420) lives in the perp market
///           extension (todo #14).
///         - No portfolio margin — cross-market offsets are todo #15;
///           this engine is a dumb store plus a decoder registry.
///
///         Mutation surface: `_store` / `_clear` are `internal` — only an
///         inheriting settlement path (todo #11 wires the `IMarket`
///         validation hooks that call `_store` after validation) may write
///         position state. Reads (`load`, `getPositionMetadata`) are open:
///         position data is public on-chain and the risk engine
///         (todo #15) reads it freely.
///
///         Typed access: because the bytes are opaque to the kernel,
///         `getPositionMetadata` delegates decoding to the per-market
///         decoder registered via `registerDecoder` ("opaque bytes with
///         typed accessors", paper line 409).
contract PositionEngine is AccessControl {
    // ---------------------------------------------------------------------
    // Roles.
    // ---------------------------------------------------------------------

    /// @notice Clearing-kernel administrator role. Gates `registerDecoder`.
    ///         `SettlementEngine` (todo #11) deployment registers each
    ///         market extension as its market's decoder under this role.
    bytes32 public constant CLEARING_ADMIN_ROLE = keccak256("CLEARING_ADMIN_ROLE");

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Position set P, keyed by trader then market ID. Values are
    ///         opaque bytes (paper/synchra.tex:409); the engine never
    ///         interprets them.
    mapping(address => mapping(bytes32 => bytes)) internal _positions;

    /// @notice Per-market decoder used by `getPositionMetadata` to read
    ///         typed metadata out of the opaque position bytes. The
    ///         decoder is the market extension itself.
    mapping(bytes32 => address) public positionDecoders;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when `trader`'s position in `marketId` is written.
    event PositionStored(address indexed trader, bytes32 indexed marketId, bytes positionData);

    /// @notice Emitted when `trader`'s position in `marketId` is deleted.
    event PositionCleared(address indexed trader, bytes32 indexed marketId);

    /// @notice Emitted when `decoder` is (re)registered for `marketId`.
    event DecoderRegistered(bytes32 indexed marketId, address indexed decoder);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice A required non-zero market ID was passed as zero.
    error ZeroMarketId();

    /// @notice `getPositionMetadata` was called for a market with no
    ///         registered decoder.
    error DecoderNotRegistered(bytes32 marketId);

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param admin Receives both `DEFAULT_ADMIN_ROLE` (role administration)
    ///              and `CLEARING_ADMIN_ROLE` (decoder registration).
    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(CLEARING_ADMIN_ROLE, admin);
    }

    // ---------------------------------------------------------------------
    // Write API (internal — settlement path only).
    // ---------------------------------------------------------------------

    /// @notice Stores `positionData` as `trader`'s position in `marketId`,
    ///         overwriting any existing position. Opaque to the kernel —
    ///         market extensions encode their own struct into bytes
    ///         (paper/synchra.tex:409).
    /// @dev    Internal: the settlement engine (todo #11) calls this from
    ///         its `IMarket`-validated trade path. Gas budget 50k covers a
    ///         fresh two-slot store plus the event; cost scales ~20k per
    ///         additional fresh 32-byte slot of payload.
    function _store(address trader, bytes32 marketId, bytes memory positionData) internal {
        if (trader == address(0)) revert ZeroAddress();
        if (marketId == bytes32(0)) revert ZeroMarketId();

        _positions[trader][marketId] = positionData;

        emit PositionStored(trader, marketId, positionData);
    }

    /// @notice Deletes `trader`'s position in `marketId`, zeroing the slot.
    /// @dev    Internal: called by the settlement path on full close
    ///         (`IMarket.validateClose`, todo #11). Deleting a slot that
    ///         was never set is a no-op apart from the event.
    function _clear(address trader, bytes32 marketId) internal {
        if (trader == address(0)) revert ZeroAddress();
        if (marketId == bytes32(0)) revert ZeroMarketId();

        delete _positions[trader][marketId];

        emit PositionCleared(trader, marketId);
    }

    // ---------------------------------------------------------------------
    // Read API.
    // ---------------------------------------------------------------------

    /// @notice Returns `trader`'s opaque position bytes for `marketId`
    ///         (paper/synchra.tex:409). Empty when no position exists.
    function load(address trader, bytes32 marketId) external view returns (bytes memory) {
        return _positions[trader][marketId];
    }

    /// @notice Returns the typed metadata of `trader`'s position in
    ///         `marketId` by delegating to the market's registered
    ///         decoder — the "typed accessor" over the opaque bytes the
    ///         risk engine reads (paper/synchra.tex:409).
    /// @dev    Reverts with `DecoderNotRegistered` when no decoder is
    ///         registered for `marketId`; decoding semantics for empty
    ///         bytes belong to the market extension's decoder.
    function getPositionMetadata(address trader, bytes32 marketId)
        external
        view
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        address decoder = positionDecoders[marketId];
        if (decoder == address(0)) revert DecoderNotRegistered(marketId);

        return IPositionDecoder(decoder).getMetadata(_positions[trader][marketId]);
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Registers `decoder` as the metadata decoder for `marketId`.
    ///         Callable only by `CLEARING_ADMIN_ROLE`; re-registration
    ///         overwrites the previous decoder.
    function registerDecoder(bytes32 marketId, address decoder) external onlyRole(CLEARING_ADMIN_ROLE) {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (decoder == address(0)) revert ZeroAddress();

        positionDecoders[marketId] = decoder;

        emit DecoderRegistered(marketId, decoder);
    }
}
