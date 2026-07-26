// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IPyth} from "pyth-sdk-solidity/IPyth.sol";
import {PythStructs} from "pyth-sdk-solidity/PythStructs.sol";

import {IOracleConsumer} from "../clearing/ICollateralEngine.sol";

/// @title  PythConsumer
/// @notice Primary-oracle consumer of the Cerdic multi-oracle stack
///         (paper/cerdic.tex:1055-1063, plan todo #12). Wraps a Pyth
///         `IPyth` deployment and exposes a staleness-checked, 1e18-scaled
///         USD price surface to the clearing kernel and the `OracleHub`.
/// @dev    MVP scope guardrails:
///         - Staleness is enforced HERE (not via Pyth's own
///           `getPriceNoOlderThan`) so every oracle source in the stack
///           fails with the same `StalePrice` surface and the same
///           `MAX_STALENESS_SECONDS = 60` bound (task spec).
///         - No ZK proofs for oracle data — explicitly out of MVP scope
///           (paper/cerdic.tex:1095-1100).
///         - No real deployment addresses are baked in: the Pyth contract
///           is wired by the admin via `setPythContract`, so tests run
///           against `MockPyth` and the Arc Testnet address is injected by
///           the deploy script (todo #30).
///
///         Price normalization: Pyth quotes as `price * 10^expo` (signed
///         fixed point, typically expo == -8). `fetchPrice` rescales to
///         the kernel-wide `1e18 == $1.00` convention documented on
///         `IOracleConsumer`.
///
///         Admin model: a single immutable `admin` gates
///         `setPythContract` / `setPriceFeedId`, reverting with
///         `NotAdmin` — mirroring `CollateralEngine` (todo #9).
contract PythConsumer is IOracleConsumer {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Maximum accepted age of a Pyth price update, in seconds.
    ///         Both oracle consumers in the stack share this bound so the
    ///         hub never compares a fresh price against a stale one.
    uint256 public constant MAX_STALENESS_SECONDS = 60;

    /// @notice Kernel price scale: `1e18 == $1.00`.
    uint256 internal constant PRICE_SCALE_DECIMALS = 18;

    /// @dev Largest decimal shift applied during normalization. `10**77`
    ///      is the largest power of ten that fits in a `uint256`
    ///      (2**256 ~ 1.15e77); anything wider would overflow.
    int256 internal constant MAX_DECIMAL_SHIFT = 77;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Consumer administrator. Gates the admin API; immutable for
    ///         the MVP (no admin-transfer surface).
    address public immutable admin;

    /// @notice The wrapped Pyth contract. Zero until wired by the admin;
    ///         `fetchPrice` reverts `PythContractNotSet` while unset.
    IPyth public pyth;

    /// @notice Collateral asset to Pyth price-feed ID registry. Backs the
    ///         `IOracleConsumer.priceOf` surface consumed by
    ///         `CollateralEngine` (todo #9). A zero value means the asset
    ///         has no feed registered.
    mapping(address => bytes32) public priceFeedIds;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when the wrapped Pyth contract is (re)set.
    event PythContractUpdated(address indexed pyth);

    /// @notice Emitted when an asset's Pyth price-feed ID is (re)set.
    event PriceFeedRegistered(address indexed asset, bytes32 indexed feedId);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the consumer admin.
    error NotAdmin();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice `fetchPrice` was called before the Pyth contract was wired.
    error PythContractNotSet();

    /// @notice The latest Pyth update for `feedId` is older than
    ///         `MAX_STALENESS_SECONDS` (or is future-dated).
    error StalePrice(bytes32 feedId, uint256 publishTime, uint256 blockTimestamp);

    /// @notice The Pyth quote is non-positive or has an exponent that
    ///         cannot be rescaled to 1e18 without overflowing.
    error InvalidPrice(bytes32 feedId, int64 price, int32 expo);

    /// @notice `priceOf` was called for an asset with no registered feed.
    error FeedNotRegistered(address asset);

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

    /// @param adminAccount Receives the immutable consumer admin. The Pyth
    ///        contract is intentionally NOT a constructor argument: the
    ///        MVP wires it post-deploy via `setPythContract` so the same
    ///        bytecode serves tests (`MockPyth`) and Arc Testnet.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
    }

    // ---------------------------------------------------------------------
    // Read API.
    // ---------------------------------------------------------------------

    /// @notice Returns the latest Pyth price for `priceFeedId`, rescaled to
    ///         1e18, together with its publish time.
    /// @dev    Reads with `getPriceUnsafe` and applies the protocol's own
    ///         staleness bound (`MAX_STALENESS_SECONDS`) so the revert
    ///         surface matches `ChainlinkConsumer`. Reverts with
    ///         `StalePrice` when the update is older than 60 seconds (or
    ///         future-dated) and with `InvalidPrice` on a non-positive or
    ///         unrescalable quote.
    /// @param  priceFeedId The Pyth price-feed ID (e.g. BTC/USD).
    /// @return price     USD price scaled to 1e18.
    /// @return updatedAt Unix timestamp of the price update.
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

    /// @inheritdoc IOracleConsumer
    /// @dev    Resolves the asset's registered Pyth feed ID and delegates
    ///         to `fetchPrice`, so the collateral engine inherits the same
    ///         staleness and validity guarantees as the hub path.
    function priceOf(address asset) external view override returns (uint256) {
        bytes32 feedId = priceFeedIds[asset];
        if (feedId == bytes32(0)) revert FeedNotRegistered(asset);
        (uint256 price,) = fetchPrice(feedId);
        return price;
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Wires (or replaces) the wrapped Pyth contract. Tests point
    ///         this at `MockPyth`; the Arc Testnet deployment address is
    ///         injected by the deploy script (todo #30) — no addresses are
    ///         hardcoded here.
    function setPythContract(address pythContract) external onlyAdmin {
        if (pythContract == address(0)) revert ZeroAddress();
        pyth = IPyth(pythContract);
        emit PythContractUpdated(pythContract);
    }

    /// @notice Registers (or clears, with `bytes32(0)`) the Pyth
    ///         price-feed ID backing `IOracleConsumer.priceOf(asset)`.
    function setPriceFeedId(address asset, bytes32 feedId) external onlyAdmin {
        if (asset == address(0)) revert ZeroAddress();
        priceFeedIds[asset] = feedId;
        emit PriceFeedRegistered(asset, feedId);
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Rescales Pyth's signed fixed-point quote (`price * 10^expo`)
    ///      to the kernel's 1e18 USD scale. The decimal shift is
    ///      `18 + expo`: multiply for shifts >= 0, divide otherwise.
    ///      `price` is guaranteed positive by the caller. Reverts
    ///      `InvalidPrice` when the shift cannot be applied with
    ///      `uint256` arithmetic (`10**78` overflows 2**256).
    function _normalizeTo1e18(bytes32 feedId, int64 rawPrice, int32 expo) internal pure returns (uint256) {
        int256 shift = int256(PRICE_SCALE_DECIMALS) + int256(expo);
        if (shift > MAX_DECIMAL_SHIFT || shift < -MAX_DECIMAL_SHIFT) {
            revert InvalidPrice(feedId, rawPrice, expo);
        }

        // casting to 'uint256' is safe because rawPrice > 0 (checked by the caller)
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 magnitude = uint256(uint64(rawPrice));
        if (shift >= 0) {
            // casting to 'uint256' is safe because shift is non-negative in this branch
            // forge-lint: disable-next-line(unsafe-typecast)
            return magnitude * (10 ** uint256(shift));
        }
        // casting to 'uint256' is safe because -shift is positive in this branch
        // forge-lint: disable-next-line(unsafe-typecast)
        return magnitude / (10 ** uint256(-shift));
    }
}
