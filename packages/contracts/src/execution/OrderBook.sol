// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IMarket} from "../clearing/IMarket.sol";
import {ICollateralEngine} from "../clearing/ICollateralEngine.sol";

/// @title  OrderBook
/// @notice On-chain order book storage for the Cerdic CLOB execution layer
///         (paper/cerdic.tex:619-629, plan todo #17). Stores limit orders
///         with EIP-712 signature verification, gas-aware modify (paper
///         line 627), cancel, and an expired-order recycle-fee claim
///         mechanism (paper line 954).
/// @dev    **Role.** This contract does NOT match orders — matching is the
///         Rust CLOB engine's role (plan todo #16). It only stores,
///         validates, and manages the lifecycle of limit orders that the
///         off-chain matching engine reads (via events / storage queries)
///         and settles through `SettlementEngine.settleTrade`.
///
///         **EIP-712.** Each order is signed by the trader's key at
///         placement. The typed data covers the order's critical parameters
///         and a per-owner nonce to prevent replay. The signature is
///         verified once at placement; subsequent modifications are
///         authenticated by `msg.sender`.
///
///         **Recycle fee.** Orders with a non-zero `expiryBlock` must
///         attach a minimum recycle fee as `msg.value`. The fee is
///         refunded on cancel or, if the order expires, claimable by
///         anyone as a cleanup incentive (paper line 954).
///
///         **Gas-aware modify.** `modifyOrder` updates the order's price
///         and size IN the existing storage slot — no delete/recreate
///         (paper line 627).
///
///         **Margin check.** During `placeOrder`, the contract calls
///         the registered market extension's `IMarket.validateOpen` to
///         verify that the trader's effective collateral (from the
///         collateral engine) covers the initial-margin requirement at
///         the current oracle price.
contract OrderBook {
    // ---------------------------------------------------------------------
    // Types.
    // ---------------------------------------------------------------------

    /// @notice Order side.
    enum Side {
        Long,
        Short
    }

    /// @notice Canonical on-chain order record.
    /// @dev    Fields are ordered so Solidity packs small types into the
    ///         first storage slot, keeping the per-order footprint at 5
    ///         slots (one packed header + four 32-byte fields).
    ///
    ///         `sigHash` is NOT stored on-chain to stay within the
    ///         placeOrder gas budget (150k). The digest is emitted in the
    ///         `OrderPlaced` event and can be verified by the off-chain
    ///         matching engine against the order parameters.
    ///
    ///         Slot layout (mapping `orders[orderId]`):
    ///           Slot 0: owner (20) + side (1) + expiryBlock (8) + pad (3)
    ///           Slot 1: marketId (32)
    ///           Slot 2: price (32)
    ///           Slot 3: size (32)
    ///           Slot 4: recycleFee (32)
    struct Order {
        address owner;
        Side side;
        uint64 expiryBlock;
        bytes32 marketId;
        uint256 price;
        int256 size;
        uint256 recycleFee;
    }

    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Minimum recycle fee for orders with a non-zero expiry.
    ///         Below this threshold, `placeOrder` reverts with
    ///         `InsufficientBond`.
    uint256 public constant MIN_RECYCLE_FEE = 0.001 ether;

    /// @notice EIP-712 domain name.
    string internal constant EIP712_DOMAIN_NAME = "Cerdic OrderBook";

    /// @notice EIP-712 domain version.
    string internal constant EIP712_DOMAIN_VERSION = "1";

    // ---------------------------------------------------------------------
    // EIP-712 typehashes.
    // ---------------------------------------------------------------------

    bytes32 internal constant EIP712_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    bytes32 internal constant ORDER_TYPEHASH =
        keccak256("Order(bytes32 marketId,Side side,uint256 price,int256 size,uint256 expiryBlock,uint256 nonce)");

    bytes32 internal constant SIDE_TYPEHASH = keccak256("Side(uint8 id)");

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Next available order ID (monotonically increasing).
    uint256 public nextOrderId;

    /// @notice Canonical order store: `orderId => Order`. Order IDs are
    ///         1-indexed; slot 0 is unused.
    mapping(uint256 => Order) public orders;

    /// @notice Per-owner nonce for EIP-712 replay protection.
    mapping(address => uint256) public nonces;

    /// @notice Address of the `PositionEngine` (or its descendant such as
    ///         `SettlementEngine` or `BtcPerpMarket`) exposing the
    ///         `positionDecoders` mapping used to resolve market extensions.
    address public immutable positionEngine;

    /// @notice Address of the `CollateralEngine` exposing
    ///         `effectiveCollateral`.
    address public immutable collateralEngine;

    /// @notice Cached EIP-712 domain separator.
    bytes32 internal immutable DOMAIN_SEPARATOR;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when `owner` places an order.
    event OrderPlaced(
        uint256 indexed orderId,
        address indexed owner,
        bytes32 indexed marketId,
        Side side,
        uint256 price,
        int256 size,
        uint256 expiryBlock,
        uint256 recycleFee,
        bytes32 digest
    );

    /// @notice Emitted when `owner` modifies an order's price and size
    ///         in-place (paper line 627).
    event OrderModified(uint256 indexed orderId, address indexed owner, uint256 newPrice, int256 newSize);

    /// @notice Emitted when `owner` cancels an order. The recycle fee is
    ///         refunded to the caller.
    event OrderCancelled(uint256 indexed orderId, address indexed owner, uint256 refundedFee);

    /// @notice Emitted when an expired order's recycle fee is claimed by
    ///         a cleaner (paper line 954).
    event OrderRecycled(uint256 indexed orderId, address indexed claimer, uint256 feeRefunded);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice The caller is not the order owner.
    error NotOwner(uint256 orderId, address caller);

    /// @notice The EIP-712 signature is invalid.
    error InvalidSignature(address recovered, address expected);

    /// @notice The trader's effective collateral cannot support this order
    ///         size at the current oracle price per the market's
    ///         `validateOpen`.
    error InsufficientMarginForOrder(uint256 effectiveCollateral);

    /// @notice The recycle fee (`msg.value`) is below `MIN_RECYCLE_FEE`.
    error InsufficientBond(uint256 value, uint256 minimum);

    /// @notice The market ID has no registered extension.
    error MarketNotSupported(bytes32 marketId);

    /// @notice The order does not exist.
    error OrderNotFound(uint256 orderId);

    /// @notice The order has expired.
    error OrderExpired(uint256 orderId, uint256 expiryBlock, uint256 currentBlock);

    /// @notice The order has not yet expired.
    error OrderNotExpired(uint256 orderId, uint256 expiryBlock, uint256 currentBlock);

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice A required non-zero `bytes32` was passed as zero.
    error ZeroMarketId();

    /// @notice Price must be non-zero.
    error ZeroPrice();

    /// @notice Size must be non-zero.
    error ZeroSize();

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param positionEngineAddr  Address of the deployed `PositionEngine`
    ///        (or its descendant) exposing the `positionDecoders` mapping.
    /// @param collateralEngineAddr  Address of the deployed
    ///        `CollateralEngine`.
    constructor(address positionEngineAddr, address collateralEngineAddr) {
        if (positionEngineAddr == address(0)) revert ZeroAddress();
        if (collateralEngineAddr == address(0)) revert ZeroAddress();

        positionEngine = positionEngineAddr;
        collateralEngine = collateralEngineAddr;

        DOMAIN_SEPARATOR = keccak256(
            abi.encode(
                EIP712_DOMAIN_TYPEHASH,
                keccak256(bytes(EIP712_DOMAIN_NAME)),
                keccak256(bytes(EIP712_DOMAIN_VERSION)),
                block.chainid,
                address(this)
            )
        );
    }

    // ---------------------------------------------------------------------
    // Write API.
    // ---------------------------------------------------------------------

    /// @notice Places a signed limit order on the book.
    /// @dev    Reverts if:
    ///         - `marketId` has no registered extension.
    ///         - The EIP-712 signature does not recover `msg.sender`.
    ///         - The market's `validateOpen` returns false for the order
    ///           size against the caller's effective collateral.
    ///         - `expiryBlock > 0` and `msg.value < MIN_RECYCLE_FEE`.
    /// @param  marketId    Kernel market identifier.
    /// @param  side        Long or Short.
    /// @param  price       Limit price (1e18-scaled USD).
    /// @param  size        Signed order size (positive = buy, negative = sell;
    ///                     1e18-scaled base units).
    /// @param  expiryBlock Block number at which the order expires
    ///                     (0 = GTC / never expires).
    /// @param  signature   EIP-712 signature over the typed order data
    ///                     (marketId, side, price, size, expiryBlock, nonce).
    /// @return orderId    The newly assigned order ID.
    function placeOrder(
        bytes32 marketId,
        Side side,
        uint256 price,
        int256 size,
        uint256 expiryBlock,
        bytes calldata signature
    ) external payable returns (uint256 orderId) {
        // --- Input validation ---
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (price == 0) revert ZeroPrice();
        if (size == 0) revert ZeroSize();
        if (expiryBlock > 0 && msg.value < MIN_RECYCLE_FEE) {
            revert InsufficientBond(msg.value, MIN_RECYCLE_FEE);
        }

        // --- Market extension check ---
        address marketExtension = _resolveMarket(marketId);
        if (marketExtension == address(0)) revert MarketNotSupported(marketId);

        // --- EIP-712 signature verification ---
        bytes32 digest;
        {
            uint256 nonce = nonces[msg.sender];
            bytes32 structHash = keccak256(abi.encode(ORDER_TYPEHASH, marketId, side, price, size, expiryBlock, nonce));
            digest = keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR, structHash));
            address recovered = _recoverSigner(digest, signature);
            if (recovered != msg.sender) revert InvalidSignature(recovered, msg.sender);
            nonces[msg.sender] = nonce + 1;
        }

        // --- Margin check ---
        {
            uint256 effCollateral = _effectiveCollateral(msg.sender);
            if (!IMarket(marketExtension).validateOpen(size, effCollateral)) {
                revert InsufficientMarginForOrder(effCollateral);
            }
        }

        // --- Store order ---
        orderId = ++nextOrderId;
        {
            Order storage order = orders[orderId];
            order.owner = msg.sender;
            order.side = side;
            order.expiryBlock = uint64(expiryBlock);
            order.marketId = marketId;
            order.price = price;
            order.size = size;
            order.recycleFee = msg.value;
        }

        emit OrderPlaced(orderId, msg.sender, marketId, side, price, size, expiryBlock, msg.value, digest);
    }

    /// @notice Modifies an existing order's price and size in-place
    ///         (gas-aware, paper line 627).
    /// @dev    Only the order's owner may modify. The order must not have
    ///         expired. Only `price` and `size` are updated — the
    ///         modification is authenticated by `msg.sender`.
    /// @param  orderId  ID of the order to modify.
    /// @param  newPrice New limit price (1e18-scaled USD).
    /// @param  newSize  New signed order size.
    function modifyOrder(uint256 orderId, uint256 newPrice, int256 newSize) external {
        Order storage order = orders[orderId];
        if (order.owner == address(0)) revert OrderNotFound(orderId);
        if (order.owner != msg.sender) revert NotOwner(orderId, msg.sender);
        if (order.expiryBlock > 0 && block.number >= order.expiryBlock) {
            revert OrderExpired(orderId, order.expiryBlock, block.number);
        }
        if (newPrice == 0) revert ZeroPrice();
        if (newSize == 0) revert ZeroSize();

        order.price = newPrice;
        order.size = newSize;

        emit OrderModified(orderId, msg.sender, newPrice, newSize);
    }

    /// @notice Cancels an order and refunds the recycle fee to the owner.
    /// @dev    Only the order's owner may cancel.
    /// @param  orderId ID of the order to cancel.
    function cancelOrder(uint256 orderId) external {
        Order storage order = orders[orderId];
        if (order.owner == address(0)) revert OrderNotFound(orderId);
        if (order.owner != msg.sender) revert NotOwner(orderId, msg.sender);

        address owner = order.owner;
        uint256 recycleFee = order.recycleFee;

        delete orders[orderId];

        if (recycleFee > 0) {
            (bool ok,) = payable(owner).call{value: recycleFee}("");
            // If the refund fails (recipient rejects ETH or contract
            // balance is low), the order is still cancelled. The
            // stranded fee is implicitly forfeit; the matching engine
            // accounts for it via the event.
            if (!ok) {
                emit OrderRecycled(orderId, owner, recycleFee);
            }
        }

        emit OrderCancelled(orderId, owner, recycleFee);
    }

    /// @notice Claims the recycle fee of an expired order as a cleanup
    ///         incentive (paper line 954). Callable by anyone.
    /// @param  orderId ID of the expired order.
    function claimRecycleFee(uint256 orderId) external {
        Order storage order = orders[orderId];
        if (order.owner == address(0)) revert OrderNotFound(orderId);
        if (order.expiryBlock == 0 || block.number < order.expiryBlock) {
            revert OrderNotExpired(orderId, order.expiryBlock, block.number);
        }

        uint256 recycleFee = order.recycleFee;

        delete orders[orderId];

        if (recycleFee > 0) {
            (bool ok,) = payable(msg.sender).call{value: recycleFee}("");
            if (!ok) {
                // Fee stranded; no recovery in this call.
            }
        }

        emit OrderRecycled(orderId, msg.sender, recycleFee);
    }

    // ---------------------------------------------------------------------
    // EIP-712 helpers.
    // ---------------------------------------------------------------------

    /// @notice Returns the EIP-712 digest for the given order parameters
    ///         and nonce. Useful for off-chain signers and tests.
    function hashOrder(bytes32 marketId, Side side, uint256 price, int256 size, uint256 expiryBlock, uint256 nonce)
        external
        view
        returns (bytes32)
    {
        bytes32 structHash = keccak256(abi.encode(ORDER_TYPEHASH, marketId, side, price, size, expiryBlock, nonce));
        return keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR, structHash));
    }

    /// @notice EIP-712 domain separator.
    function domainSeparator() external view returns (bytes32) {
        return DOMAIN_SEPARATOR;
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Recovers the signer address from an EIP-712 digest and
    ///      signature. Supports 65-byte (r,s,v) and 64-byte (r,s, v
    ///      encoded in s high bit, EIP-2098) signatures.
    function _recoverSigner(bytes32 hash, bytes calldata signature) internal pure returns (address) {
        if (signature.length == 65) {
            bytes32 r;
            bytes32 s;
            uint8 v;
            assembly {
                r := calldataload(signature.offset)
                s := calldataload(add(signature.offset, 0x20))
                v := byte(0, calldataload(add(signature.offset, 0x40)))
            }
            if (v < 27) v += 27;
            return ecrecover(hash, v, r, s);
        } else if (signature.length == 64) {
            bytes32 r;
            bytes32 s;
            assembly {
                r := calldataload(signature.offset)
                s := calldataload(add(signature.offset, 0x20))
            }
            uint8 v = 27 + uint8(uint256(s) >> 255);
            s = bytes32((uint256(s) << 1) >> 1);
            return ecrecover(hash, v, r, s);
        }
        revert InvalidSignatureLength(signature.length);
    }

    /// @dev Resolves the market extension address for `marketId` via
    ///      the `PositionEngine.positionDecoders` mapping.
    function _resolveMarket(bytes32 marketId) internal view returns (address) {
        (bool ok, bytes memory data) =
            positionEngine.staticcall(abi.encodeWithSignature("positionDecoders(bytes32)", marketId));
        if (!ok || data.length < 32) return address(0);
        return abi.decode(data, (address));
    }

    /// @dev Reads `effectiveCollateral(trader)` from the collateral engine.
    function _effectiveCollateral(address trader) internal view returns (uint256) {
        (bool ok, bytes memory data) =
            collateralEngine.staticcall(abi.encodeWithSignature("effectiveCollateral(address)", trader));
        if (!ok || data.length < 32) return 0;
        return abi.decode(data, (uint256));
    }

    /// @notice Emitted by `_recoverSigner` when the signature length is
    ///         neither 64 nor 65 bytes.
    error InvalidSignatureLength(uint256 length);
}
