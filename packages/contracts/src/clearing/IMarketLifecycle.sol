// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IMarket} from "./IMarket.sol";

/// @title IMarketLifecycle
/// @notice Hooks the kernel calls around position/funding/liquidation/oracle events so a
///         market extension can keep its own derived state in lock-step with the kernel.
/// @dev    Hooks run inside the kernel's settlement transaction; a reverting hook reverts
///         the whole settlement. Hook targets are admin-registered, so no reentrancy guard.
interface IMarketLifecycle {
    /// @notice Reverting vetoes the open.
    function beforeOpenPosition(address user, bytes32 market, int256 size, uint256 price) external;

    function afterOpenPosition(address user, bytes32 market, IMarket.MarketPosition calldata position) external;

    /// @notice Reverting vetoes the close.
    function beforeClosePosition(address user, bytes32 market, IMarket.MarketPosition calldata position) external;

    function afterClosePosition(address user, bytes32 market, int256 pnl) external;

    function beforeSettleFunding(bytes32 market, int256 rate) external;

    function onLiquidation(address user, bytes32 market, IMarket.MarketPosition calldata position) external;

    function onOracleUpdate(bytes32 market, uint256 price) external;
}

/// @title ISealedMarketLifecycle
/// @notice The settleMatch (privacy-preserving) counterpart to IMarketLifecycle. No
///         size/price/side parameters, by design: settleMatch never puts them on-chain,
///         so this hook can't leak them either. A market extension that wants to track
///         per-portfolioKey state (e.g. a funding-index checkpoint) for privately-settled
///         positions implements this; extensions that don't care about the private path
///         can skip it.
interface ISealedMarketLifecycle {
    /// @notice Called once per leg from SettlementEngine.settleMatch, after the sealed
    ///         position and collateral delta are stored.
    function onSealedOpen(bytes32 portfolioKey, bytes32 marketId) external;
}
