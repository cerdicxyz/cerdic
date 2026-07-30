// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IPyth} from "pyth-sdk-solidity/IPyth.sol";
import {PythStructs} from "pyth-sdk-solidity/PythStructs.sol";

import {IOracleConsumer} from "../clearing/ICollateralEngine.sol";

/// @title  PythConsumer
/// @notice Wraps Pyth IPyth, exposes a staleness-checked 1e18-scaled USD price surface.
/// @dev    Staleness (60s) is enforced here rather than via Pyth's own getPriceNoOlderThan,
///         so every oracle in the stack shares one StalePrice surface. Pyth quotes as
///         price * 10^expo; fetchPrice rescales to the kernel's 1e18 == $1.00 convention.
contract PythConsumer is IOracleConsumer {
    uint256 public constant MAX_STALENESS_SECONDS = 60;
    uint256 internal constant PRICE_SCALE_DECIMALS = 18;

    /// @dev 10**77 is the largest power of ten that fits in a uint256.
    int256 internal constant MAX_DECIMAL_SHIFT = 77;

    address public immutable admin;

    /// @notice Zero until wired; fetchPrice reverts PythContractNotSet while unset.
    IPyth public pyth;

    mapping(address => bytes32) public priceFeedIds;

    event PythContractUpdated(address indexed pyth);
    event PriceFeedRegistered(address indexed asset, bytes32 indexed feedId);

    error NotAdmin();
    error ZeroAddress();
    error PythContractNotSet();
    error StalePrice(bytes32 feedId, uint256 publishTime, uint256 blockTimestamp);
    error InvalidPrice(bytes32 feedId, int64 price, int32 expo);
    error FeedNotRegistered(address asset);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    /// @dev Pyth contract wired post-deploy via setPythContract, not a constructor arg,
    ///      so the same bytecode serves tests (MockPyth) and the real deployment.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
    }

    function fetchPrice(bytes32 priceFeedId) public view returns (uint256 price, uint256 updatedAt) {
        IPyth source = pyth;
        if (address(source) == address(0)) revert PythContractNotSet();

        PythStructs.Price memory quote = source.getPriceUnsafe(priceFeedId);

        uint256 publishTime = quote.publishTime;
        if (publishTime == 0 || block.timestamp < publishTime) {
            revert StalePrice(priceFeedId, publishTime, block.timestamp);
        }
        if (block.timestamp - publishTime > MAX_STALENESS_SECONDS) {
            revert StalePrice(priceFeedId, publishTime, block.timestamp);
        }
        if (quote.price <= 0) {
            revert InvalidPrice(priceFeedId, quote.price, quote.expo);
        }

        return (_normalizeTo1e18(priceFeedId, quote.price, quote.expo), publishTime);
    }

    function priceOf(address asset) external view override returns (uint256) {
        bytes32 feedId = priceFeedIds[asset];
        if (feedId == bytes32(0)) revert FeedNotRegistered(asset);
        (uint256 price,) = fetchPrice(feedId);
        return price;
    }

    function setPythContract(address pythContract) external onlyAdmin {
        if (pythContract == address(0)) revert ZeroAddress();
        pyth = IPyth(pythContract);
        emit PythContractUpdated(pythContract);
    }

    function setPriceFeedId(address asset, bytes32 feedId) external onlyAdmin {
        if (asset == address(0)) revert ZeroAddress();
        priceFeedIds[asset] = feedId;
        emit PriceFeedRegistered(asset, feedId);
    }

    /// @dev Decimal shift = 18 + expo: multiply for shifts >= 0, divide otherwise.
    function _normalizeTo1e18(bytes32 feedId, int64 rawPrice, int32 expo) internal pure returns (uint256) {
        int256 shift = int256(PRICE_SCALE_DECIMALS) + int256(expo);
        if (shift > MAX_DECIMAL_SHIFT || shift < -MAX_DECIMAL_SHIFT) {
            revert InvalidPrice(feedId, rawPrice, expo);
        }

        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 magnitude = uint256(uint64(rawPrice));
        if (shift >= 0) {
            // forge-lint: disable-next-line(unsafe-typecast)
            return magnitude * (10 ** uint256(shift));
        }
        // forge-lint: disable-next-line(unsafe-typecast)
        return magnitude / (10 ** uint256(-shift));
    }
}
