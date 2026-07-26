// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";

import {ICollateralBalanceSource} from "./ICollateralEngine.sol";
import {IRiskMonitor} from "./RiskMonitor.sol";

/// @title  Account
/// @notice Clearing account contract of the Cerdic clearing kernel
///         (paper/cerdic.tex:339-421). Owns the authoritative per-trader
///         account tuple `A = (B, P, H)` (paper lines 356-360):
///         collateral balances by asset, the position set keyed by market
///         (stored as opaque bytes per paper line 409 so the kernel stays
///         market-agnostic), and the credit/debit record for funding
///         payments, fees, and liquidation penalties.
/// @dev    MVP scope guardrails:
///         - The isolated-margin withdraw guard lives in todo #15's
///           `RiskMonitor.isWithdrawSafe`: `withdraw` consults the wired
///           monitor after the balance check and reverts
///           `InsufficientMarginForWithdraw` when the withdrawal would push
///           the account below its maintenance-margin requirement. While
///           no monitor is wired (`riskMonitor == 0`, the bootstrap state)
///           the check is skipped.
///         - No liquidation execution — todo #13 (`LiquidationEntry`) only
///           consumes `freezeAccount`.
///         - The account state machine reduces to Healthy/Frozen for the
///           MVP; the full liquidation state machine lives in todo #13.
///
///         Position and credit-record writes are NOT exposed here: the
///         position engine (todo #10) and settlement engine (todo #11) own
///         those mutation paths. This contract only owns collateral custody
///         plus the read surface the collateral engine (todo #9) consumes
///         via `ICollateralBalanceSource`.
///
///         Reentrancy: deposit/withdraw follow checks-effects-interactions
///         (balance is mutated before the token transfer), so no
///         ReentrancyGuard is paid for — keeps `deposit`/`withdraw` inside
///         the 80k/100k gas budgets.
contract Account is AccessControl, ICollateralBalanceSource {
    using SafeERC20 for IERC20;

    // ---------------------------------------------------------------------
    // Roles.
    // ---------------------------------------------------------------------

    /// @notice Clearing-kernel administrator role. Gates `freezeAccount`.
    ///         `LiquidationEntry` (todo #13) will be granted this role to
    ///         freeze flagged accounts.
    bytes32 public constant CLEARING_ADMIN_ROLE = keccak256("CLEARING_ADMIN_ROLE");

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Per-trader account tuple `A = (B, P, H)`
    ///         (paper/cerdic.tex:356-360) plus the MVP frozen flag.
    /// @dev    `frozen` doubles as the struct's only scalar member: a
    ///         public mapping to a struct containing ONLY mappings cannot
    ///         generate a getter (solc 6744), so the auto-generated
    ///         `accounts(trader)` getter returns `frozen` — the account's
    ///         frozen status. Mapping members are read through the
    ///         explicit view functions below.
    struct AccountData {
        /// @dev B — collateral balance per asset (token base units).
        mapping(address => uint256) collateralBalances;
        /// @dev P — position set keyed by market ID, opaque bytes per
        ///      paper/cerdic.tex:409. Written by the position/settlement
        ///      engines, not by this contract.
        mapping(bytes32 => bytes) positions;
        /// @dev H — credit/debit record per market ID for funding
        ///      payments, fees, and liquidation penalties.
        mapping(bytes32 => int256) creditRecords;
        /// @dev Frozen accounts cannot deposit or withdraw. Set by
        ///      `CLEARING_ADMIN_ROLE` via `freezeAccount` (consumed by the
        ///      liquidation flow in todo #13).
        bool frozen;
    }

    /// @notice Authoritative account store, keyed by trader wallet address.
    ///         The generated getter returns the account's `frozen` flag.
    mapping(address => AccountData) public accounts;

    /// @notice Risk monitor consulted by `withdraw` (todo #15). The zero
    ///         address means unwired: withdrawals skip the isolated-margin
    ///         check (bootstrap state until the deploy script wires it).
    IRiskMonitor public riskMonitor;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when `trader` deposits `amount` of `asset`.
    event CollateralDeposited(address indexed trader, address indexed asset, uint256 amount);

    /// @notice Emitted when `trader` withdraws `amount` of `asset`.
    event CollateralWithdrawn(address indexed trader, address indexed asset, uint256 amount);

    /// @notice Emitted when `trader`'s account is frozen by an admin.
    event AccountFrozen(address indexed trader);

    /// @notice Emitted when `amount` of `asset` is seized from `trader`'s
    ///         frozen account and credited to `recipient`'s collateral
    ///         balance (liquidation penalty path, todo #13).
    event CollateralSeized(address indexed trader, address indexed asset, uint256 amount, address indexed recipient);

    /// @notice Emitted when the risk monitor consulted by `withdraw` is
    ///         (re)wired; the zero address unwires it.
    event RiskMonitorUpdated(address indexed monitor);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Withdraw requested more than the available collateral balance.
    error InsufficientCollateral(address trader, address asset, uint256 requested, uint256 available);

    /// @notice Withdraw would push the account below its isolated
    ///         maintenance-margin requirement (todo #15:
    ///         `RiskMonitor.isWithdrawSafe` returned false).
    error InsufficientMarginForWithdraw(address trader, address asset, uint256 amount);

    /// @notice Deposit/withdraw attempted on a frozen account.
    error AccountIsFrozen(address trader);

    /// @notice Collateral seizure attempted on an account that is not
    ///         frozen — seizure is a liquidation-path operation only.
    error AccountNotFrozen(address trader);

    /// @notice A required non-zero amount was passed as zero.
    error ZeroAmount();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param admin Receives both `DEFAULT_ADMIN_ROLE` (role administration)
    ///              and `CLEARING_ADMIN_ROLE` (freeze/unfreeze operations).
    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(CLEARING_ADMIN_ROLE, admin);
    }

    // ---------------------------------------------------------------------
    // Collateral API.
    // ---------------------------------------------------------------------

    /// @notice Deposits `amount` of `asset` into the caller's clearing
    ///         account. Pulls the tokens from the caller via
    ///         `transferFrom`, so the caller must have approved this
    ///         contract for at least `amount` beforehand.
    /// @dev    Reverts when the caller's account is frozen, when `asset`
    ///         is the zero address, or when `amount` is zero.
    function deposit(address asset, uint256 amount) external {
        if (asset == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (accounts[msg.sender].frozen) revert AccountIsFrozen(msg.sender);

        accounts[msg.sender].collateralBalances[asset] += amount;

        IERC20(asset).safeTransferFrom(msg.sender, address(this), amount);

        emit CollateralDeposited(msg.sender, asset, amount);
    }

    /// @notice Withdraws `amount` of `asset` from the caller's clearing
    ///         account back to the caller's wallet.
    /// @dev    Reverts with `InsufficientCollateral` when the balance is
    ///         insufficient, with `AccountIsFrozen` when the caller's
    ///         account is frozen, and with `InsufficientMarginForWithdraw`
    ///         when the wired risk monitor (todo #15) reports the
    ///         withdrawal would breach the isolated maintenance-margin
    ///         requirement. The margin check is a pure read placed before
    ///         the balance mutation (checks-effects-interactions); it is
    ///         skipped while no monitor is wired.
    function withdraw(address asset, uint256 amount) external {
        if (amount == 0) revert ZeroAmount();
        if (accounts[msg.sender].frozen) revert AccountIsFrozen(msg.sender);

        AccountData storage account = accounts[msg.sender];
        uint256 balance = account.collateralBalances[asset];
        if (balance < amount) {
            revert InsufficientCollateral(msg.sender, asset, amount, balance);
        }

        IRiskMonitor monitor = riskMonitor;
        if (address(monitor) != address(0) && !monitor.isWithdrawSafe(msg.sender, asset, amount)) {
            revert InsufficientMarginForWithdraw(msg.sender, asset, amount);
        }

        account.collateralBalances[asset] = balance - amount;

        IERC20(asset).safeTransfer(msg.sender, amount);

        emit CollateralWithdrawn(msg.sender, asset, amount);
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Seizes `amount` of `asset` from `trader`'s FROZEN account and
    ///         credits it to `recipient`'s collateral balance. This is the
    ///         liquidation penalty path anticipated by the credit-record
    ///         docstring above (todo #13): `LiquidationEntry` sweeps the
    ///         1% penalty out of a flagged account to the liquidator and
    ///         the insurance fund.
    /// @dev    The seized collateral never leaves this contract's custody —
    ///         it moves between two in-kernel balances, so total escrowed
    ///         collateral is preserved by construction. Reverts with
    ///         `AccountNotFrozen` unless the account is in the liquidation
    ///         (frozen) state, and with `InsufficientCollateral` when the
    ///         balance cannot cover the seizure.
    function seizeCollateral(address trader, address asset, uint256 amount, address recipient)
        external
        onlyRole(CLEARING_ADMIN_ROLE)
    {
        if (asset == address(0) || recipient == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (!accounts[trader].frozen) revert AccountNotFrozen(trader);

        AccountData storage account = accounts[trader];
        uint256 balance = account.collateralBalances[asset];
        if (balance < amount) {
            revert InsufficientCollateral(trader, asset, amount, balance);
        }

        account.collateralBalances[asset] = balance - amount;
        accounts[recipient].collateralBalances[asset] += amount;

        emit CollateralSeized(trader, asset, amount, recipient);
    }

    /// @notice Wires (or unwires) the risk monitor consulted by `withdraw`
    ///         (todo #15). Passing the zero address unwires it, returning
    ///         `withdraw` to the bootstrap balance-only check.
    function setRiskMonitor(address monitor) external onlyRole(CLEARING_ADMIN_ROLE) {
        riskMonitor = IRiskMonitor(monitor);
        emit RiskMonitorUpdated(monitor);
    }

    /// @notice Freezes `trader`'s account, blocking further deposits and
    ///         withdrawals. Callable only by `CLEARING_ADMIN_ROLE`; the
    ///         liquidation entry point (todo #13) calls this when an
    ///         account breaches the liquidation threshold.
    /// @dev    Idempotent: re-freezing an already-frozen account is a
    ///         no-op and emits no event.
    function freezeAccount(address trader) external onlyRole(CLEARING_ADMIN_ROLE) {
        if (trader == address(0)) revert ZeroAddress();
        if (accounts[trader].frozen) return;

        accounts[trader].frozen = true;

        emit AccountFrozen(trader);
    }

    // ---------------------------------------------------------------------
    // Read API.
    // ---------------------------------------------------------------------

    /// @notice Returns the caller's opaque position bytes for `marketId`
    ///         (paper/cerdic.tex:409). Empty when no position exists.
    function getPosition(bytes32 marketId) external view returns (bytes memory) {
        return accounts[msg.sender].positions[marketId];
    }

    /// @notice Returns the caller's collateral balance of `asset` (token
    ///         base units).
    function getCollateralBalance(address asset) external view returns (uint256) {
        return accounts[msg.sender].collateralBalances[asset];
    }

    /// @inheritdoc ICollateralBalanceSource
    /// @notice Read surface consumed by the collateral engine (todo #9)
    ///         when computing `C_eff = Σ b_a · (1 − h_a) · p_a`
    ///         (paper/cerdic.tex:384-386).
    function collateralBalanceOf(address trader, address asset) external view returns (uint256) {
        return accounts[trader].collateralBalances[asset];
    }
}
