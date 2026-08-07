// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";

import {Account as ClearingAccount} from "./Account.sol";
import {ICollateralEngine} from "./ICollateralEngine.sol";

/// @title  Vault
/// @notice A firm/principal-funded strategy vault (paper/cerdic.tex sec:yield, the
///         firm-funded shape): a share-accounted pool that trades through the clearing
///         kernel under its own address, exactly like any other account.
/// @dev    The vault contract IS the trader identity the kernel sees: `Account`,
///         `RiskMonitor`, and `CapabilityRegistry` all key off `address(this)`. A
///         capability granted to `address(this)` in `CapabilityRegistry` pins the
///         vault's execution limits the same way it would for a prop account (composing
///         the two primitives is what makes this the firm-funded shape rather than a
///         bare pool). If that capability breaches, `CapabilityRegistry` freezes this
///         vault's `Account` record directly, which blocks `deposit`/`withdraw` below
///         through `Account`'s own `AccountIsFrozen` guard, no extra wiring needed here.
///
///         This is deliberately NOT the public/permissionless vault shape (third-party
///         depositors funding a manager they don't know, the Hyperliquid Vaults / GMX
///         GLP pattern). That shape needs a ZK solvency attestation to stay coherent with
///         sealed positions, and no such circuit exists yet, so it is not implemented
///         here. This vault's depositors DO see NAV (via `totalAssets`), which is fine
///         for a firm funding its own trader but would defeat the point of TEE-sealed
///         positions if opened to arbitrary public depositors.
///
///         NAV tracks the vault's REALIZED effective collateral (`CollateralEngine`),
///         not unrealized position PnL, matching the isolated-margin-only MVP scope
///         elsewhere in the kernel (RiskMonitor's cross-market-offset scope-out).
contract Vault is ERC20 {
    using SafeERC20 for IERC20;

    uint256 internal constant SCALE = 1e18;
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Cap on the performance fee: 5,000 bps (50%).
    uint256 public constant MAX_PERFORMANCE_FEE_BPS = 5_000;

    IERC20 public immutable asset;
    ClearingAccount public immutable account;
    ICollateralEngine public immutable collateralEngine;

    address public immutable admin;
    address public feeRecipient;
    uint256 public performanceFeeBps;

    /// @notice High-water mark, 1e18-scaled NAV per share. Fees crystallize only on NEW
    ///         highs, never on a recovery back up to a previous high.
    uint256 public highWaterMarkPerShare;

    event Deposited(address indexed depositor, address indexed receiver, uint256 assets, uint256 shares);
    event Withdrawn(address indexed owner, address indexed receiver, uint256 assets, uint256 shares);
    event PerformanceFeeAccrued(uint256 feeAssets, uint256 feeShares, uint256 newHighWaterMarkPerShare);
    event FeeRecipientUpdated(address indexed recipient);
    event PerformanceFeeUpdated(uint256 feeBps);

    error ZeroAddress();
    error ZeroAmount();
    error NotAdmin();
    error FeeTooHigh(uint256 feeBps);
    error InsufficientShares(uint256 requested, uint256 available);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    constructor(
        string memory name_,
        string memory symbol_,
        address adminAccount,
        address asset_,
        address account_,
        address collateralEngine_,
        address feeRecipient_,
        uint256 performanceFeeBps_
    ) ERC20(name_, symbol_) {
        if (adminAccount == address(0) || asset_ == address(0) || account_ == address(0)) {
            revert ZeroAddress();
        }
        if (collateralEngine_ == address(0) || feeRecipient_ == address(0)) revert ZeroAddress();
        if (performanceFeeBps_ > MAX_PERFORMANCE_FEE_BPS) revert FeeTooHigh(performanceFeeBps_);

        admin = adminAccount;
        asset = IERC20(asset_);
        account = ClearingAccount(account_);
        collateralEngine = ICollateralEngine(collateralEngine_);
        feeRecipient = feeRecipient_;
        performanceFeeBps = performanceFeeBps_;
        highWaterMarkPerShare = SCALE;
    }

    /// @notice The vault's NAV: realized effective collateral held under this vault's
    ///         own trader identity in the clearing kernel.
    function totalAssets() public view returns (uint256) {
        return collateralEngine.effectiveCollateral(address(this));
    }

    /// @notice Deposits `assets` of the vault's asset, mints shares to `receiver` at the
    ///         current NAV per share, and routes the capital into the clearing kernel
    ///         under this vault's own trader identity.
    function deposit(uint256 assets, address receiver) external returns (uint256 shares) {
        if (assets == 0) revert ZeroAmount();
        if (receiver == address(0)) revert ZeroAddress();

        _accrueFee();

        uint256 supply = totalSupply();
        uint256 assetsBefore = totalAssets();
        shares = supply == 0 ? assets : (assets * supply) / assetsBefore;
        if (shares == 0) revert ZeroAmount();

        asset.safeTransferFrom(msg.sender, address(this), assets);
        asset.forceApprove(address(account), assets);
        account.deposit(address(asset), assets);

        _mint(receiver, shares);
        emit Deposited(msg.sender, receiver, assets, shares);
    }

    /// @notice Burns `shares` from the caller, withdraws the corresponding assets out of
    ///         the clearing kernel, and sends them to `receiver`. Subject to the same
    ///         margin check as any other account: a withdrawal that would breach the
    ///         vault's margin requirement on open positions reverts in `Account`, exactly
    ///         as it would for a directly-held account.
    function withdraw(uint256 shares, address receiver) external returns (uint256 assets) {
        if (shares == 0) revert ZeroAmount();
        if (receiver == address(0)) revert ZeroAddress();
        uint256 callerBalance = balanceOf(msg.sender);
        if (shares > callerBalance) revert InsufficientShares(shares, callerBalance);

        _accrueFee();

        uint256 supply = totalSupply();
        assets = (shares * totalAssets()) / supply;

        _burn(msg.sender, shares);
        account.withdraw(address(asset), assets);
        asset.safeTransfer(receiver, assets);

        emit Withdrawn(msg.sender, receiver, assets, shares);
    }

    /// @notice NAV per share, 1e18-scaled. `SCALE` (1.0) while the vault is empty.
    function navPerShare() public view returns (uint256) {
        uint256 supply = totalSupply();
        if (supply == 0) return SCALE;
        return (totalAssets() * SCALE) / supply;
    }

    function setFeeRecipient(address recipient) external onlyAdmin {
        if (recipient == address(0)) revert ZeroAddress();
        feeRecipient = recipient;
        emit FeeRecipientUpdated(recipient);
    }

    function setPerformanceFeeBps(uint256 feeBps) external onlyAdmin {
        if (feeBps > MAX_PERFORMANCE_FEE_BPS) revert FeeTooHigh(feeBps);
        performanceFeeBps = feeBps;
        emit PerformanceFeeUpdated(feeBps);
    }

    /// @dev Crystallizes the performance fee on any NAV-per-share high above the
    ///      high-water mark: mints dilutive fee shares to `feeRecipient` for the
    ///      appreciation since the last high, then raises the mark. A supply of zero
    ///      resets the mark to 1.0 rather than accruing against an undefined NAV.
    function _accrueFee() internal {
        uint256 supply = totalSupply();
        if (supply == 0) {
            highWaterMarkPerShare = SCALE;
            return;
        }

        uint256 assets = totalAssets();
        uint256 currentNavPerShare = (assets * SCALE) / supply;
        if (currentNavPerShare <= highWaterMarkPerShare) {
            return;
        }

        uint256 gainPerShare = currentNavPerShare - highWaterMarkPerShare;
        uint256 profit = (gainPerShare * supply) / SCALE;
        uint256 feeAssets = (profit * performanceFeeBps) / BPS_DENOMINATOR;
        if (feeAssets == 0) {
            highWaterMarkPerShare = currentNavPerShare;
            return;
        }

        // Dilutive mint at the current price: feeShares assets are worth feeAssets at
        // today's NAV per share.
        uint256 feeShares = (feeAssets * supply) / assets;
        _mint(feeRecipient, feeShares);

        // Recompute the mark against the diluted supply so the fee is not re-charged
        // on the same appreciation next call.
        highWaterMarkPerShare = (assets * SCALE) / totalSupply();
        emit PerformanceFeeAccrued(feeAssets, feeShares, highWaterMarkPerShare);
    }
}
