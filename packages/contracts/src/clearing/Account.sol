// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AccessControl} from "openzeppelin-contracts/contracts/access/AccessControl.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";

import {ICollateralBalanceSource} from "./ICollateralEngine.sol";
import {IRiskMonitor} from "./RiskMonitor.sol";

/// @title  Account
/// @notice Owns collateral custody, positions, and credit records per trader.
/// @dev    Position/credit-record writes belong to PositionEngine/SettlementEngine, not here.
///         deposit/withdraw follow checks-effects-interactions, no ReentrancyGuard needed.
contract Account is AccessControl, ICollateralBalanceSource {
    using SafeERC20 for IERC20;

    /// @notice Gates freezeAccount; granted to LiquidationEntry.
    bytes32 public constant CLEARING_ADMIN_ROLE = keccak256("CLEARING_ADMIN_ROLE");

    /// @dev `frozen` is the only scalar field: a public mapping to a struct of only mappings
    ///      can't generate a getter, so the auto getter returns `frozen`.
    struct AccountData {
        mapping(address => uint256) collateralBalances;
        mapping(bytes32 => bytes) positions;
        mapping(bytes32 => int256) creditRecords;
        bool frozen;
    }

    mapping(address => AccountData) public accounts;

    /// @notice Zero address = unwired, withdrawals skip the margin check.
    IRiskMonitor public riskMonitor;

    event CollateralDeposited(address indexed trader, address indexed asset, uint256 amount);
    event CollateralWithdrawn(address indexed trader, address indexed asset, uint256 amount);
    event AccountFrozen(address indexed trader);
    event CollateralSeized(address indexed trader, address indexed asset, uint256 amount, address indexed recipient);
    event RiskMonitorUpdated(address indexed monitor);

    error InsufficientCollateral(address trader, address asset, uint256 requested, uint256 available);
    error InsufficientMarginForWithdraw(address trader, address asset, uint256 amount);
    error AccountIsFrozen(address trader);
    error AccountNotFrozen(address trader);
    error ZeroAmount();
    error ZeroAddress();

    constructor(address admin) {
        if (admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(CLEARING_ADMIN_ROLE, admin);
    }

    /// @notice Pulls `amount` of `asset` via transferFrom; caller must have approved this contract.
    function deposit(address asset, uint256 amount) external {
        if (asset == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (accounts[msg.sender].frozen) revert AccountIsFrozen(msg.sender);

        accounts[msg.sender].collateralBalances[asset] += amount;

        IERC20(asset).safeTransferFrom(msg.sender, address(this), amount);

        emit CollateralDeposited(msg.sender, asset, amount);
    }

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

    /// @notice Liquidation penalty path: moves collateral between two in-kernel balances,
    ///         never leaves custody. Only callable on a frozen account.
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

    function setRiskMonitor(address monitor) external onlyRole(CLEARING_ADMIN_ROLE) {
        riskMonitor = IRiskMonitor(monitor);
        emit RiskMonitorUpdated(monitor);
    }

    /// @dev Idempotent: re-freezing is a no-op, no event.
    function freezeAccount(address trader) external onlyRole(CLEARING_ADMIN_ROLE) {
        if (trader == address(0)) revert ZeroAddress();
        if (accounts[trader].frozen) return;

        accounts[trader].frozen = true;

        emit AccountFrozen(trader);
    }

    function getPosition(bytes32 marketId) external view returns (bytes memory) {
        return accounts[msg.sender].positions[marketId];
    }

    function getCollateralBalance(address asset) external view returns (uint256) {
        return accounts[msg.sender].collateralBalances[asset];
    }

    function collateralBalanceOf(address trader, address asset) external view returns (uint256) {
        return accounts[trader].collateralBalances[asset];
    }
}
