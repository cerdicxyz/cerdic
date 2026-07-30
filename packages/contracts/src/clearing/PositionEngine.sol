// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";

/// @title IPositionDecoder
/// @notice Per-market decoder that reads typed metadata out of a position's opaque bytes.
///         Implemented by each market extension; the kernel never interprets position bytes itself.
interface IPositionDecoder {
    function getMetadata(bytes calldata positionData)
        external
        view
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage);
}

/// @title  PositionEngine
/// @notice Stores each account's positions as opaque bytes; market extensions own the encoding.
/// @dev    `_store`/`_clear` are internal — only an inheriting settlement path may write.
contract PositionEngine is AccessControl {
    bytes32 public constant CLEARING_ADMIN_ROLE = keccak256("CLEARING_ADMIN_ROLE");

    /// @notice trader => marketId => opaque position bytes.
    mapping(address => mapping(bytes32 => bytes)) internal _positions;

    /// @notice marketId => decoder (the market extension itself).
    mapping(bytes32 => address) public positionDecoders;

    event PositionStored(address indexed trader, bytes32 indexed marketId, bytes positionData);
    event PositionCleared(address indexed trader, bytes32 indexed marketId);
    event DecoderRegistered(bytes32 indexed marketId, address indexed decoder);

    error ZeroAddress();
    error ZeroMarketId();
    error DecoderNotRegistered(bytes32 marketId);

    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(CLEARING_ADMIN_ROLE, admin);
    }

    /// @dev Overwrites any existing position for `trader`/`marketId`.
    function _store(address trader, bytes32 marketId, bytes memory positionData) internal {
        if (trader == address(0)) revert ZeroAddress();
        if (marketId == bytes32(0)) revert ZeroMarketId();

        _positions[trader][marketId] = positionData;

        emit PositionStored(trader, marketId, positionData);
    }

    function _clear(address trader, bytes32 marketId) internal {
        if (trader == address(0)) revert ZeroAddress();
        if (marketId == bytes32(0)) revert ZeroMarketId();

        delete _positions[trader][marketId];

        emit PositionCleared(trader, marketId);
    }

    /// @notice Empty bytes when no position exists.
    function load(address trader, bytes32 marketId) external view returns (bytes memory) {
        return _positions[trader][marketId];
    }

    function getPositionMetadata(address trader, bytes32 marketId)
        external
        view
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        address decoder = positionDecoders[marketId];
        if (decoder == address(0)) revert DecoderNotRegistered(marketId);

        return IPositionDecoder(decoder).getMetadata(_positions[trader][marketId]);
    }

    /// @notice Re-registration overwrites the previous decoder.
    function registerDecoder(bytes32 marketId, address decoder) external onlyRole(CLEARING_ADMIN_ROLE) {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (decoder == address(0)) revert ZeroAddress();

        positionDecoders[marketId] = decoder;

        emit DecoderRegistered(marketId, decoder);
    }
}
