// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;

/// @title IOracleConsumer
/// @notice Price source the collateral engine reads from. Stub mode returns 1e18 per asset
///         until PythConsumer is wired.
interface IOracleConsumer {
    /// @notice USD price of `asset`, scaled to 1e18.
    function priceOf(address asset) external view returns (uint256);
}

/// @title ICollateralBalanceSource
/// @notice Read-only balance surface. Implemented by Account.sol, the authoritative store.
interface ICollateralBalanceSource {
    function collateralBalanceOf(address trader, address asset) external view returns (uint256);
}

/// @title ICollateralEngine
/// @notice Collateral-tier registry and effective-collateral valuation.
/// @dev    Static MVP tiers: Tier 1 USDC/EURC 0%, Tier 2 USYC 200bps, Tier 3 10-20%, Tier 4 15-35%.
///         Dynamic haircuts are out of scope for now.
interface ICollateralEngine {
    event AssetRegistered(address indexed asset, uint8 tier, uint16 haircutBps);
    event OracleUpdated(address indexed oracle);
    event BalanceSourceUpdated(address indexed source);

    error NotAdmin();
    error AssetNotRegistered(address asset);
    error InvalidTier(uint8 tier);
    error HaircutOutOfRange(uint8 tier, uint16 haircutBps);
    error ZeroAddress();
    error BalanceSourceNotSet();

    function tierOf(address asset) external view returns (uint8);
    function haircutBpsOf(address asset) external view returns (uint16);

    /// @notice Stub mode returns 1e18 until the oracle is wired.
    function oraclePriceOf(address asset) external view returns (uint256);

    /// @notice `amount * (1 - haircut) * price`, 1e18-scaled USD.
    function assetValueUsd(address asset, uint256 amount) external view returns (uint256);

    /// @notice Sum of assetValueUsd over trader's registered-asset balances.
    function effectiveCollateral(address trader) external view returns (uint256);

    /// @notice Haircut must lie inside the tier's static MVP range.
    function registerAsset(address asset, uint8 tier, uint16 haircutBps) external;
}
