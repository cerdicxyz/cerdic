// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {PositionEngine} from "./PositionEngine.sol";
import {IMarket} from "./IMarket.sol";
import {IMarketLifecycle, ISealedMarketLifecycle} from "./IMarketLifecycle.sol";
import {MarketImpactTwap} from "../oracle/MarketImpactTwap.sol";
import {AttestationRouter} from "./AttestationRouter.sol";

/// @title  SettlementEngine
/// @notice Settles matched trades: validates each side against its market extension,
///         applies collateral + position updates atomically, forwards any upfront premium.
///         Also exposes settleMatch, the TEE-private path: side/price/size never appear
///         in calldata, only an attested TEE may call it, and it trusts the TEE's
///         collateral delta rather than recomputing margin from plaintext.
/// @dev    settleTrade order: beforeOpenPosition hooks -> margin validation -> both
///         positions stored -> afterOpenPosition hooks -> premium transfer -> impact-TWAP
///         feed. A revert at any step unwinds all prior state changes.
contract SettlementEngine is PositionEngine {
    /// @notice Role permitted to submit matched trades for settlement.
    bytes32 public constant SETTLER_ROLE = keccak256("SETTLER_ROLE");

    /// @notice Role permitted to settle position close-outs (liquidation path).
    bytes32 public constant LIQUIDATOR_ROLE = keccak256("LIQUIDATOR_ROLE");

    uint256 internal constant SCALE = 1e18;
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Initial margin requirement in basis points; mirrors ProtocolConstants.IMR_BPS.
    uint256 internal constant IMR_BPS = 500;

    /// @notice Per-market leverage ceiling; mirrors ProtocolConstants.MAX_LEVERAGE_BPS / 100.
    uint256 internal constant LEVERAGE_CEILING = 20;

    /// @notice Zero until wired; while zero, settlement skips the impact-TWAP feed.
    MarketImpactTwap public impactTwap;

    /// @notice Gates settleMatch to attested TEE callers. Zero until wired.
    AttestationRouter public attestationRouter;

    /// @dev One sealed position per portfolioKey per market. collateral is plaintext
    ///      (the kernel's own solvency bound, see settleMatch); sealedParams (side,
    ///      leverage, entryPrice, size, TP/SL) is opaque, only the TEE ever decrypts it.
    struct SealedPosition {
        bytes sealedParams;
        int256 collateral;
    }

    mapping(bytes32 => mapping(bytes32 => SealedPosition)) internal _sealedPositions;

    /// @notice matchId replay guard — the kernel's own "no double-settlement" invariant.
    mapping(bytes32 => bool) public settledMatches;

    event TradeSettled(
        bytes32 indexed marketId,
        address indexed longTrader,
        address indexed shortTrader,
        int256 size,
        uint256 price,
        uint256 premium
    );
    event ImpactTwapUpdated(address indexed impactTwap);
    event PositionCloseSettled(address indexed trader, bytes32 indexed marketId, int256 remainingSize);
    event AttestationRouterUpdated(address indexed router);
    event MatchSettled(bytes32 indexed matchId);

    error MarketNotRegistered(bytes32 marketId);
    error InsufficientMargin(address trader, int256 size, uint256 margin);
    error IncorrectPremium(uint256 value, uint256 premium);
    error PremiumTransferFailed(address shortTrader, uint256 premium);
    error SameTrader(address trader);
    error NonPositiveSize(int256 size);
    error ZeroPrice();
    error AttestationRouterNotSet();
    error NotAuthorizedTEE(address caller);
    error ZeroMatchId();
    error ZeroPortfolioKey();
    error MatchAlreadySettled(bytes32 matchId);
    error InsufficientSealedCollateral(bytes32 portfolioKey, bytes32 marketId, int256 wouldBe);

    constructor(address admin) PositionEngine(admin) {
        _grantRole(SETTLER_ROLE, admin);
        _grantRole(LIQUIDATOR_ROLE, admin);
    }

    /// @param size Long-side size (positive); short side is the negation.
    /// @param premium Upfront premium for premium-bearing instruments, must equal msg.value.
    ///        Zero for margin-based instruments (perps, FX).
    function settleTrade(
        bytes32 marketId,
        address longTrader,
        address shortTrader,
        int256 size,
        uint256 price,
        uint256 premium
    ) external payable onlyRole(SETTLER_ROLE) {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (longTrader == address(0) || shortTrader == address(0)) revert ZeroAddress();
        if (longTrader == shortTrader) revert SameTrader(longTrader);
        if (size <= 0) revert NonPositiveSize(size);
        if (price == 0) revert ZeroPrice();

        address market = positionDecoders[marketId];
        if (market == address(0)) revert MarketNotRegistered(marketId);

        IMarketLifecycle(market).beforeOpenPosition(longTrader, marketId, size, price);
        IMarketLifecycle(market).beforeOpenPosition(shortTrader, marketId, -size, price);

        uint256 margin = requiredMargin(size, price);
        if (!IMarket(market).validateOpen(size, margin)) {
            revert InsufficientMargin(longTrader, size, margin);
        }
        if (!IMarket(market).validateOpen(-size, margin)) {
            revert InsufficientMargin(shortTrader, -size, margin);
        }

        IMarket.MarketPosition memory longPosition = IMarket.MarketPosition({
            marketId: marketId, size: size, entryPrice: price, margin: margin, leverage: LEVERAGE_CEILING
        });
        IMarket.MarketPosition memory shortPosition = IMarket.MarketPosition({
            marketId: marketId, size: -size, entryPrice: price, margin: margin, leverage: LEVERAGE_CEILING
        });
        _store(longTrader, marketId, abi.encode(longPosition));
        _store(shortTrader, marketId, abi.encode(shortPosition));

        IMarketLifecycle(market).afterOpenPosition(longTrader, marketId, longPosition);
        IMarketLifecycle(market).afterOpenPosition(shortTrader, marketId, shortPosition);

        if (msg.value != premium) revert IncorrectPremium(msg.value, premium);
        if (premium > 0) {
            (bool ok,) = payable(shortTrader).call{value: premium}("");
            if (!ok) revert PremiumTransferFailed(shortTrader, premium);
        }

        MarketImpactTwap impact = impactTwap;
        if (address(impact) != address(0)) {
            // forge-lint: disable-next-line(unsafe-typecast)
            impact.recordTrade(marketId, price, uint256(size));
        }

        emit TradeSettled(marketId, longTrader, shortTrader, size, price, premium);
    }

    function setImpactTwap(address twap) external onlyRole(CLEARING_ADMIN_ROLE) {
        impactTwap = MarketImpactTwap(twap);
        emit ImpactTwapUpdated(twap);
    }

    function setAttestationRouter(address router) external onlyRole(CLEARING_ADMIN_ROLE) {
        attestationRouter = AttestationRouter(router);
        emit AttestationRouterUpdated(router);
    }

    /// @notice TEE-private settlement, per docs/spec-contracts-tee.md section 2.2: the TEE
    ///         recomputes margin in-enclave and calls this with its result, the kernel never
    ///         reads side/price/size — it only checks the caller is attested and that its
    ///         own collateral-bound and no-double-settlement invariants hold.
    /// @param  collateralDeltaA/B Signed change to each side's tracked collateral; the TEE's
    ///         margin computation, applied without re-derivation.
    /// @param  sealedParamsA/B Opaque, TEE-encrypted (side, leverage, entryPrice, size, TP/SL).
    function settleMatch(
        bytes32 matchId,
        bytes32 marketId,
        bytes32 portfolioKeyA,
        int256 collateralDeltaA,
        bytes calldata sealedParamsA,
        bytes32 portfolioKeyB,
        int256 collateralDeltaB,
        bytes calldata sealedParamsB
    ) external {
        AttestationRouter router = attestationRouter;
        if (address(router) == address(0)) revert AttestationRouterNotSet();
        if (!router.isAuthorizedTEE(msg.sender)) revert NotAuthorizedTEE(msg.sender);

        if (matchId == bytes32(0)) revert ZeroMatchId();
        if (settledMatches[matchId]) revert MatchAlreadySettled(matchId);
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (portfolioKeyA == bytes32(0) || portfolioKeyB == bytes32(0)) revert ZeroPortfolioKey();

        address market = positionDecoders[marketId];
        if (market == address(0)) revert MarketNotRegistered(marketId);

        settledMatches[matchId] = true;

        _applySealedLeg(marketId, portfolioKeyA, collateralDeltaA, sealedParamsA);
        _applySealedLeg(marketId, portfolioKeyB, collateralDeltaB, sealedParamsB);

        // No size/price/side passed, see ISealedMarketLifecycle: this only lets the
        // market checkpoint per-portfolioKey state (e.g. a funding-index stamp).
        ISealedMarketLifecycle(market).onSealedOpen(portfolioKeyA, marketId);
        ISealedMarketLifecycle(market).onSealedOpen(portfolioKeyB, marketId);

        emit MatchSettled(matchId);
    }

    /// @notice Opaque sealedParams plus the plain running collateral for portfolioKey/marketId.
    function loadSealed(bytes32 portfolioKey, bytes32 marketId)
        external
        view
        returns (bytes memory sealedParams, int256 collateral)
    {
        SealedPosition storage position = _sealedPositions[portfolioKey][marketId];
        return (position.sealedParams, position.collateral);
    }

    /// @dev Collateral bound is the kernel's own invariant (paper/cerdic.tex:369): it never
    ///      goes negative, regardless of what the TEE's delta claims.
    function _applySealedLeg(
        bytes32 marketId,
        bytes32 portfolioKey,
        int256 collateralDelta,
        bytes calldata sealedParams
    ) internal {
        SealedPosition storage position = _sealedPositions[portfolioKey][marketId];
        int256 newCollateral = position.collateral + collateralDelta;
        if (newCollateral < 0) revert InsufficientSealedCollateral(portfolioKey, marketId, newCollateral);

        position.collateral = newCollateral;
        position.sealedParams = sealedParams;
    }

    /// @dev The offsetting takeover trade is settled separately via settleTrade, which
    ///      overwrites both sides' records (doesn't net) — this corrects the liquidated
    ///      trader's record afterward. Keeps the original entry price.
    function settlePositionClose(
        address trader,
        bytes32 marketId,
        int256 remainingSize,
        uint256 entryPrice,
        uint256 remainingMargin
    ) external onlyRole(LIQUIDATOR_ROLE) {
        if (remainingSize == 0) {
            _clear(trader, marketId);
        } else {
            IMarket.MarketPosition memory remaining = IMarket.MarketPosition({
                marketId: marketId,
                size: remainingSize,
                entryPrice: entryPrice,
                margin: remainingMargin,
                leverage: LEVERAGE_CEILING
            });
            _store(trader, marketId, abi.encode(remaining));
        }

        emit PositionCloseSettled(trader, marketId, remainingSize);
    }

    /// @notice `|size| * price * IMR_BPS / (1e18 * 1e4)`; 5% of notional = 20x leverage ceiling.
    function requiredMargin(int256 size, uint256 price) public pure returns (uint256) {
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = size > 0 ? uint256(size) : uint256(-size);
        return absSize * price * IMR_BPS / (SCALE * BPS_DENOMINATOR);
    }
}
