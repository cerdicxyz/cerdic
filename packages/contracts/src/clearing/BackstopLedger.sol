// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";

/// @title IAttestationRouter
/// @notice Subset of AttestationRouter this contract needs: which addresses
///         are authorized TEE attesters. Same interface RiskMonitor.sol
///         already depends on, not redefined differently here.
interface IAttestationRouter {
    function isAuthorizedTEE(address tee) external view returns (bool);
}

/// @title  BackstopLedger
/// @notice The on-chain home for the kernel-owned backstop maker's PnL
///         (`crates/cerdic-tee-matcher::backstop::BackstopState`, and
///         `crates/risk::backstop_maker::PnlLedger` for the 1e18-scaled
///         mirror): a TEE-attested record of what the backstop has made or
///         lost per market, plus a funded reserve that actually pays out
///         the loss side of that number.
/// @dev    `simulations/backstop-maker/dmm_stipend_sim.py` found the thing
///         this contract exists to fix: even the fully-guarded backstop
///         loses money to ordinary informed flow, not just attackers, and
///         until now that cost had nowhere real to be tracked or funded,
///         it just accrued silently in enclave memory. `submitState`
///         mirrors that in-memory ledger 1:1 so the loss is visible and
///         auditable, not a number only the TEE operator could see;
///         `fundReserve`/`drawSubsidy` give it an actual funding path
///         instead of just a number nobody backs.
///
///         Deliberately NOT margin-enforcing: this contract only tracks
///         and pays out the backstop's OWN PnL, it has no bearing on any
///         trader's margin requirement (that's `RiskMonitor.sol`, a
///         separate concern even though both reuse the same
///         `AttestationRouter` authorization pattern).
contract BackstopLedger {
    using SafeERC20 for IERC20;

    address public immutable admin;

    /// @notice The asset the reserve is funded in (e.g. USDC).
    IERC20 public immutable asset;

    /// @notice Zero = unset; submitState reverts AttestationRouterNotSet.
    IAttestationRouter public attestationRouter;

    /// @dev Mirrors `BackstopState`'s two fields exactly: `cumulativePnl`
    ///      (signed, 1e18-scaled USD) and `inventory` (signed base units).
    struct MarketState {
        int256 cumulativePnl;
        int256 inventory;
        uint64 lastUpdated;
    }

    mapping(bytes32 => MarketState) public marketState;

    /// @notice Funded capital available to cover attested losses, summed
    ///         across every market (not partitioned per-market: the same
    ///         capital backs whichever market currently needs it, matching
    ///         how a single TEE process holds all markets' backstop state).
    uint256 public reserve;

    event StateAttested(bytes32 indexed marketId, int256 cumulativePnl, int256 inventory, address indexed attester);
    event ReserveFunded(address indexed funder, uint256 amount);
    event SubsidyDrawn(bytes32 indexed marketId, address indexed recipient, uint256 amount);
    event AttestationRouterUpdated(address indexed router);

    error NotAdmin();
    error ZeroAddress();
    error ZeroAmount();
    error AttestationRouterNotSet();
    error NotAuthorizedAttester();
    error InsufficientReserve(uint256 requested, uint256 available);
    error AmountExceedsSubsidyOwed(uint256 requested, uint256 owed);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    constructor(address adminAccount, address asset_) {
        if (adminAccount == address(0) || asset_ == address(0)) revert ZeroAddress();
        admin = adminAccount;
        asset = IERC20(asset_);
    }

    function setAttestationRouter(address router) external onlyAdmin {
        attestationRouter = IAttestationRouter(router);
        emit AttestationRouterUpdated(router);
    }

    /// @notice A TEE authorized via `attestationRouter` reports the backstop
    ///         maker's current cumulative PnL and inventory for `marketId`.
    ///         Overwrites the prior state entirely, there is no partial
    ///         update, matching the non-retroactivity pattern elsewhere in
    ///         the kernel: a new attestation is the new truth, not a delta.
    function submitState(bytes32 marketId, int256 cumulativePnl, int256 inventory) external {
        IAttestationRouter router = attestationRouter;
        if (address(router) == address(0)) revert AttestationRouterNotSet();
        if (!router.isAuthorizedTEE(msg.sender)) revert NotAuthorizedAttester();

        marketState[marketId] =
            MarketState({cumulativePnl: cumulativePnl, inventory: inventory, lastUpdated: uint64(block.timestamp)});
        emit StateAttested(marketId, cumulativePnl, inventory, msg.sender);
    }

    /// @notice The break-even subsidy owed for `marketId`: zero if the
    ///         attested PnL is net non-negative, otherwise the size of the
    ///         net loss. Mirrors `PnlLedger::breakeven_subsidy_needed`
    ///         exactly, only losses are ever owed, a profitable market
    ///         never owes anything back.
    function breakevenSubsidyNeeded(bytes32 marketId) public view returns (uint256) {
        int256 pnl = marketState[marketId].cumulativePnl;
        return pnl >= 0 ? 0 : uint256(-pnl);
    }

    /// @notice Anyone can top up the reserve backing the backstop maker's
    ///         losses (an insurance fund, a market-maker DAO treasury,
    ///         whatever capital a deployment chooses, this contract is
    ///         deliberately agnostic about who funds it).
    function fundReserve(uint256 amount) external {
        if (amount == 0) revert ZeroAmount();
        asset.safeTransferFrom(msg.sender, address(this), amount);
        reserve += amount;
        emit ReserveFunded(msg.sender, amount);
    }

    /// @notice Admin draws up to the attested subsidy for `marketId` out of
    ///         the reserve, paying `recipient` (whatever real capital
    ///         actually backs the backstop's fills, e.g. a `Vault.sol`
    ///         instance). Capped at what's actually owed: draining more
    ///         than the attested loss would be paying out money the
    ///         backstop never actually lost.
    function drawSubsidy(bytes32 marketId, address recipient, uint256 amount) external onlyAdmin {
        if (recipient == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();

        uint256 owed = breakevenSubsidyNeeded(marketId);
        if (amount > owed) revert AmountExceedsSubsidyOwed(amount, owed);
        if (amount > reserve) revert InsufficientReserve(amount, reserve);

        reserve -= amount;
        // The draw reduces the market's outstanding loss by exactly what
        // was paid out, so a second draw for the same loss is impossible:
        // the next `breakevenSubsidyNeeded` read reflects what's left.
        marketState[marketId].cumulativePnl += int256(amount);
        asset.safeTransfer(recipient, amount);
        emit SubsidyDrawn(marketId, recipient, amount);
    }
}
