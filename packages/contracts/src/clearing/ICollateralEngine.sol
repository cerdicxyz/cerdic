// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;

/// @title IOracleConsumer
/// @notice Minimal price-source surface the collateral engine reads from.
///         Implemented by `PythConsumer.sol` (todo #12); until that is wired
///         the engine runs in stub mode and returns `1e18` per asset.
/// @dev    Prices are USD-denominated and scaled to 1e18
///         (i.e. `1e18 == $1.00`).
interface IOracleConsumer {
    /// @notice Returns the USD price of `asset`, scaled to 1e18.
    function priceOf(address asset) external view returns (uint256);
}

/// @title ICollateralBalanceSource
/// @notice Read-only surface the collateral engine uses to pull per-trader,
///         per-asset collateral balances when computing
///         `C_eff = Σ b_a · (1 − h_a) · p_a` (paper/synchra.tex:384-386).
/// @dev    Implemented by `Account.sol` (todo #8), which owns the
///         authoritative `collateralBalances` storage. The engine never
///         custody-splits balances itself; it only reads.
interface ICollateralBalanceSource {
    /// @notice Returns trader's deposited balance of `asset` (token base
    ///         units, 1e18-scaled for the MVP stablecoin basket).
    function collateralBalanceOf(address trader, address asset) external view returns (uint256);
}

/// @title ICollateralEngine
/// @notice Collateral-tier registry and effective-collateral valuation for
///         the Synchra clearing kernel (paper/synchra.tex:362-386).
/// @dev    Static MVP tier table per paper Table 1 (lines 367-380):
///         Tier 1 USDC/EURC 0%, Tier 2 USYC 2%-5% (MVP pins 200 bps),
///         Tier 3 10%-20%, Tier 4 15%-35%. Dynamic haircuts are Phase 1
///         (paper line 1166) and explicitly out of scope.
interface ICollateralEngine {
    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when an asset is (re)registered with a tier/haircut.
    event AssetRegistered(address indexed asset, uint8 tier, uint16 haircutBps);

    /// @notice Emitted when the oracle price source is (re)set.
    event OracleUpdated(address indexed oracle);

    /// @notice Emitted when the collateral balance source is (re)set.
    event BalanceSourceUpdated(address indexed source);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the engine admin.
    error NotAdmin();

    /// @notice Queried or valued an asset that is not tier-registered.
    error AssetNotRegistered(address asset);

    /// @notice Tier outside the paper's 1-4 taxonomy.
    error InvalidTier(uint8 tier);

    /// @notice Haircut outside the static MVP range for its tier
    ///         (paper/synchra.tex:367-380).
    error HaircutOutOfRange(uint8 tier, uint16 haircutBps);

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice `effectiveCollateral` was called before a balance source
    ///         (Account.sol) was wired in.
    error BalanceSourceNotSet();

    // ---------------------------------------------------------------------
    // Read API.
    // ---------------------------------------------------------------------

    /// @notice Returns the collateral tier (1-4) of a registered asset.
    function tierOf(address asset) external view returns (uint8);

    /// @notice Returns the haircut (basis points) of a registered asset.
    function haircutBpsOf(address asset) external view returns (uint16);

    /// @notice Returns the oracle USD price of `asset` (1e18-scaled).
    ///         Stub mode returns `1e18` until the oracle is wired (todo #12).
    function oraclePriceOf(address asset) external view returns (uint256);

    /// @notice USD value of `amount` of `asset` after its haircut:
    ///         `amount · (1 − h_a) · p_a` (1e18-scaled USD).
    function assetValueUsd(address asset, uint256 amount) external view returns (uint256);

    /// @notice Effective collateral of `trader`:
    ///         `C_eff = Σ b_a · (1 − h_a) · p_a` over registered assets
    ///         (paper/synchra.tex:384-386). 1e18-scaled USD.
    function effectiveCollateral(address trader) external view returns (uint256);

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Registers (or updates) an asset's tier and haircut. The
    ///         haircut must lie inside the tier's static MVP range.
    function registerAsset(address asset, uint8 tier, uint16 haircutBps) external;
}
