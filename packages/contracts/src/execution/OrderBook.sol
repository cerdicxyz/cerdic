// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IMarket} from "../clearing/IMarket.sol";
import {ICollateralEngine} from "../clearing/ICollateralEngine.sol";

/// @title  OrderBook
/// @notice On-chain limit order storage: EIP-712 signed placement, in-place modify,
///         cancel, and an expired-order recycle-fee claim.
/// @dev    Does NOT match orders — that's the off-chain Rust CLOB engine's role. This
///         only stores/validates/manages order lifecycle; matching reads it via events.
///         Recycle fee: orders with expiryBlock > 0 must attach MIN_RECYCLE_FEE as
///         msg.value, refunded on cancel or claimable by anyone once expired.
contract OrderBook {
    enum Side {
        Long,
        Short
    }

    /// @dev Field order packs owner+side+expiryBlock into one slot (5 slots/order total).
    ///      sigHash is not stored (gas budget); it's emitted in OrderPlaced instead.
    struct Order {
        address owner;
        Side side;
        uint64 expiryBlock;
        bytes32 marketId;
        uint256 price;
        int256 size;
        uint256 recycleFee;
    }

    uint256 public constant MIN_RECYCLE_FEE = 0.001 ether;
    string internal constant EIP712_DOMAIN_NAME = "Cerdic OrderBook";
    string internal constant EIP712_DOMAIN_VERSION = "1";

    bytes32 internal constant EIP712_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 internal constant ORDER_TYPEHASH =
        keccak256("Order(bytes32 marketId,Side side,uint256 price,int256 size,uint256 expiryBlock,uint256 nonce)");
    bytes32 internal constant SIDE_TYPEHASH = keccak256("Side(uint8 id)");

    uint256 public nextOrderId;

    /// @notice 1-indexed; slot 0 unused.
    mapping(uint256 => Order) public orders;
    mapping(address => uint256) public nonces;

    /// @notice PositionEngine (or descendant) exposing positionDecoders.
    address public immutable positionEngine;
    address public immutable collateralEngine;
    bytes32 internal immutable DOMAIN_SEPARATOR;

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
    event OrderModified(uint256 indexed orderId, address indexed owner, uint256 newPrice, int256 newSize);
    event OrderCancelled(uint256 indexed orderId, address indexed owner, uint256 refundedFee);
    event OrderRecycled(uint256 indexed orderId, address indexed claimer, uint256 feeRefunded);

    error NotOwner(uint256 orderId, address caller);
    error InvalidSignature(address recovered, address expected);
    error InsufficientMarginForOrder(uint256 effectiveCollateral);
    error InsufficientBond(uint256 value, uint256 minimum);
    error MarketNotSupported(bytes32 marketId);
    error OrderNotFound(uint256 orderId);
    error OrderExpired(uint256 orderId, uint256 expiryBlock, uint256 currentBlock);
    error OrderNotExpired(uint256 orderId, uint256 expiryBlock, uint256 currentBlock);
    error ZeroAddress();
    error ZeroMarketId();
    error ZeroPrice();
    error ZeroSize();

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

    /// @param size Signed (positive = buy, negative = sell).
    /// @param expiryBlock 0 = GTC.
    function placeOrder(
        bytes32 marketId,
        Side side,
        uint256 price,
        int256 size,
        uint256 expiryBlock,
        bytes calldata signature
    ) external payable returns (uint256 orderId) {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (price == 0) revert ZeroPrice();
        if (size == 0) revert ZeroSize();
        if (expiryBlock > 0 && msg.value < MIN_RECYCLE_FEE) {
            revert InsufficientBond(msg.value, MIN_RECYCLE_FEE);
        }

        address marketExtension = _resolveMarket(marketId);
        if (marketExtension == address(0)) revert MarketNotSupported(marketId);

        bytes32 digest;
        {
            uint256 nonce = nonces[msg.sender];
            bytes32 structHash = keccak256(abi.encode(ORDER_TYPEHASH, marketId, side, price, size, expiryBlock, nonce));
            digest = keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR, structHash));
            address recovered = _recoverSigner(digest, signature);
            if (recovered != msg.sender) revert InvalidSignature(recovered, msg.sender);
            nonces[msg.sender] = nonce + 1;
        }

        {
            uint256 effCollateral = _effectiveCollateral(msg.sender);
            if (!IMarket(marketExtension).validateOpen(size, effCollateral)) {
                revert InsufficientMarginForOrder(effCollateral);
            }
        }

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

    /// @notice Updates price/size in place; authenticated by msg.sender, not re-signed.
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

    function cancelOrder(uint256 orderId) external {
        Order storage order = orders[orderId];
        if (order.owner == address(0)) revert OrderNotFound(orderId);
        if (order.owner != msg.sender) revert NotOwner(orderId, msg.sender);

        address owner = order.owner;
        uint256 recycleFee = order.recycleFee;

        delete orders[orderId];

        if (recycleFee > 0) {
            (bool ok,) = payable(owner).call{value: recycleFee}("");
            // Cancel still succeeds on refund failure; the stranded fee is forfeit.
            if (!ok) {
                emit OrderRecycled(orderId, owner, recycleFee);
            }
        }

        emit OrderCancelled(orderId, owner, recycleFee);
    }

    /// @notice Cleanup incentive for expired orders; callable by anyone.
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

    function hashOrder(bytes32 marketId, Side side, uint256 price, int256 size, uint256 expiryBlock, uint256 nonce)
        external
        view
        returns (bytes32)
    {
        bytes32 structHash = keccak256(abi.encode(ORDER_TYPEHASH, marketId, side, price, size, expiryBlock, nonce));
        return keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR, structHash));
    }

    function domainSeparator() external view returns (bytes32) {
        return DOMAIN_SEPARATOR;
    }

    /// @dev Supports 65-byte (r,s,v) and 64-byte EIP-2098 signatures.
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

    function _resolveMarket(bytes32 marketId) internal view returns (address) {
        (bool ok, bytes memory data) =
            positionEngine.staticcall(abi.encodeWithSignature("positionDecoders(bytes32)", marketId));
        if (!ok || data.length < 32) return address(0);
        return abi.decode(data, (address));
    }

    function _effectiveCollateral(address trader) internal view returns (uint256) {
        (bool ok, bytes memory data) =
            collateralEngine.staticcall(abi.encodeWithSignature("effectiveCollateral(address)", trader));
        if (!ok || data.length < 32) return 0;
        return abi.decode(data, (uint256));
    }

    error InvalidSignatureLength(uint256 length);
}
