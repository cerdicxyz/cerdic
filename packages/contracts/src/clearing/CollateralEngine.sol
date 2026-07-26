// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {ICollateralEngine, ICollateralBalanceSource, IOracleConsumer} from "./ICollateralEngine.sol";
import {ProtocolConstants} from "../lib/ProtocolConstants.sol";

/// @title  CollateralEngine
/// @notice Collateral engine of the Cerdic clearing kernel
///         (paper/cerdic.tex:362-386). Owns the four-tier collateral
///         classification (paper Table 1, lines 367-380) and the
///         effective-collateral valuation
///         `C_eff = Σ b_a · (1 − h_a) · p_a` (paper lines 384-386).
/// @dev    MVP scope guardrails:
///         - Static haircuts only. Dynamic, risk-engine-adjusted haircuts
///           are Phase 1 (paper/cerdic.tex:378, line 1166) and explicitly
///           out of scope; `registerAsset` validates every haircut against
///           the static per-tier ranges pinned in `ProtocolConstants`.
///         - Stub oracle. `oraclePriceOf` returns `1e18` ($1.00) for every
///           asset until `PythConsumer` (todo #12) is wired via
///           `setOracle`; the MVP stablecoin basket (USDC/USYC) prices at
///           $1.00 by construction.
///         - No custody. Balances are read from `Account.sol` (todo #8)
///           through `ICollateralBalanceSource`; the engine never holds
///           tokens.
///
///         Admin model: a single immutable `admin` gates `registerAsset` /
///         `setOracle` / `setBalanceSource`, reverting with `NotAdmin` —
///         matching the error surface declared in `ICollateralEngine`.
///         The deploy script (todo #30) is the expected admin.
contract CollateralEngine is ICollateralEngine, ProtocolConstants {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Basis-point denominator (100.00%).
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Oracle price scale: `1e18 == $1.00`.
    uint256 internal constant PRICE_SCALE = 1e18;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Engine administrator. Gates the admin API; immutable for the
    ///         MVP (no admin-transfer surface in `ICollateralEngine`).
    address public immutable admin;

    /// @notice Oracle price source. The zero address means stub mode: every
    ///         asset prices at `PRICE_SCALE` ($1.00). Wired to
    ///         `PythConsumer` (todo #12) via `setOracle`.
    IOracleConsumer public oracle;

    /// @notice Collateral balance source (`Account.sol`, todo #8). Must be
    ///         wired via `setBalanceSource` before `effectiveCollateral`
    ///         is callable.
    ICollateralBalanceSource public balanceSource;

    /// @dev Collateral tier (1-4) per asset; 0 means not registered, so the
    ///      tier mapping doubles as the registration marker.
    mapping(address => uint8) internal _tiers;

    /// @dev Haircut (bps) per asset; meaningful only when `_tiers[a] != 0`.
    mapping(address => uint16) internal _haircutBps;

    /// @dev Registered asset list in registration order — the summation
    ///      domain A of the `C_eff` formula (paper/cerdic.tex:385).
    address[] internal _assets;

    // ---------------------------------------------------------------------
    // Modifiers.
    // ---------------------------------------------------------------------

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param adminAccount Receives the immutable engine admin.
    /// @param usdcAsset    USDC token address, registered as the MVP Tier-1
    ///                     asset at 0% haircut (paper Table 1, line 371).
    /// @dev    Static MVP registration (plan todo #9): Tier 1 = USDC at 0
    ///         bps, Tier 2 = USYC at 200 bps (the 2%-5% range floor). USYC
    ///         is pinned to the Arc Testnet address in
    ///         `ProtocolConstants.USYC_ARC_TESTNET`; Tier 3/4 assets are
    ///         NOT pre-registered in the MVP.
    constructor(address adminAccount, address usdcAsset) {
        if (adminAccount == address(0)) revert ZeroAddress();
        if (usdcAsset == address(0)) revert ZeroAddress();

        admin = adminAccount;

        // casting to 'uint16' is safe because T1_HAIRCUT_BPS == 0
        // forge-lint: disable-next-line(unsafe-typecast)
        _registerAsset(usdcAsset, 1, uint16(T1_HAIRCUT_BPS));
        // casting to 'uint16' is safe because T2_HAIRCUT_BPS_MIN == 200 < 2**16
        // forge-lint: disable-next-line(unsafe-typecast)
        _registerAsset(USYC_ARC_TESTNET, 2, uint16(T2_HAIRCUT_BPS_MIN));
    }

    // ---------------------------------------------------------------------
    // Read API.
    // ---------------------------------------------------------------------

    /// @inheritdoc ICollateralEngine
    function tierOf(address asset) external view override returns (uint8) {
        uint8 tier = _tiers[asset];
        if (tier == 0) revert AssetNotRegistered(asset);
        return tier;
    }

    /// @inheritdoc ICollateralEngine
    function haircutBpsOf(address asset) external view override returns (uint16) {
        if (_tiers[asset] == 0) revert AssetNotRegistered(asset);
        return _haircutBps[asset];
    }

    /// @inheritdoc ICollateralEngine
    function oraclePriceOf(address asset) external view override returns (uint256) {
        return _priceOf(asset);
    }

    /// @inheritdoc ICollateralEngine
    /// @dev    `amount · (10_000 − h_a) · p_a / (10_000 · 1e18)`: the
    ///         haircut is applied to the balance before pricing, matching
    ///         the paper's `b_a · (1 − h_a) · p_a` term. `public` (not
    ///         `external`) so `effectiveCollateral` calls it as an internal
    ///         jump instead of paying for a CALL per asset.
    function assetValueUsd(address asset, uint256 amount) public view override returns (uint256) {
        if (_tiers[asset] == 0) revert AssetNotRegistered(asset);
        return amount * (BPS_DENOMINATOR - _haircutBps[asset]) * _priceOf(asset) / (BPS_DENOMINATOR * PRICE_SCALE);
    }

    /// @inheritdoc ICollateralEngine
    /// @dev    Iterates the registered-asset list (the summation domain A),
    ///         skips zero balances, and accumulates the per-asset
    ///         haircut-adjusted USD value. Gas budget: ≤120k with 4 assets
    ///         (plan todo #9) — each iteration costs one cold SLOAD for the
    ///         list entry plus one `collateralBalanceOf` call.
    function effectiveCollateral(address trader) external view override returns (uint256) {
        ICollateralBalanceSource source = balanceSource;
        if (address(source) == address(0)) revert BalanceSourceNotSet();

        uint256 total;
        uint256 count = _assets.length;
        for (uint256 i; i < count; ++i) {
            address asset = _assets[i];
            uint256 balance = source.collateralBalanceOf(trader, asset);
            if (balance == 0) continue;
            total += assetValueUsd(asset, balance);
        }
        return total;
    }

    /// @notice Registered collateral asset list in registration order — the
    ///         summation domain A of the `C_eff` formula
    ///         (paper/cerdic.tex:385), exposed so the liquidation entry
    ///         (todo #13) can sweep a flagged account's collateral for the
    ///         liquidation penalty without a parallel registry.
    function registeredAssets() external view returns (address[] memory) {
        return _assets;
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @inheritdoc ICollateralEngine
    function registerAsset(address asset, uint8 tier, uint16 haircutBps) external override onlyAdmin {
        _registerAsset(asset, tier, haircutBps);
    }

    /// @notice Wires (or clears) the oracle price source. Passing the zero
    ///         address returns the engine to stub mode ($1.00 per asset),
    ///         which is the MVP default until `PythConsumer` (todo #12)
    ///         lands.
    function setOracle(address oracle_) external onlyAdmin {
        oracle = IOracleConsumer(oracle_);
        emit OracleUpdated(oracle_);
    }

    /// @notice Wires the collateral balance source (`Account.sol`,
    ///         todo #8). Must be called before `effectiveCollateral`.
    function setBalanceSource(address source) external onlyAdmin {
        if (source == address(0)) revert ZeroAddress();
        balanceSource = ICollateralBalanceSource(source);
        emit BalanceSourceUpdated(source);
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Registers (or re-registers) `asset`, validating the haircut
    ///      against the tier's static MVP range. First-time registration
    ///      appends to the `C_eff` summation domain; re-registration
    ///      updates tier/haircut in place WITHOUT duplicating the entry,
    ///      so an asset is never summed twice.
    function _registerAsset(address asset, uint8 tier, uint16 haircutBps) internal {
        if (asset == address(0)) revert ZeroAddress();
        _checkTierHaircut(tier, haircutBps);

        if (_tiers[asset] == 0) {
            _assets.push(asset);
        }
        _tiers[asset] = tier;
        _haircutBps[asset] = haircutBps;

        emit AssetRegistered(asset, tier, haircutBps);
    }

    /// @dev Validates `tier` is inside the paper's 1-4 taxonomy and
    ///      `haircutBps` inside the tier's static MVP range
    ///      (paper/cerdic.tex:367-380; ranges from `ProtocolConstants`).
    ///      Assignment-based branches (no literal `return 0;`) keep every
    ///      line visible to `forge coverage` (see notepad learning #7).
    function _checkTierHaircut(uint8 tier, uint16 haircutBps) internal pure {
        uint256 minBps;
        uint256 maxBps;
        if (tier == 1) {
            minBps = T1_HAIRCUT_BPS;
            maxBps = T1_HAIRCUT_BPS;
        } else if (tier == 2) {
            minBps = T2_HAIRCUT_BPS_MIN;
            maxBps = T2_HAIRCUT_BPS_MAX;
        } else if (tier == 3) {
            minBps = T3_HAIRCUT_BPS_MIN;
            maxBps = T3_HAIRCUT_BPS_MAX;
        } else if (tier == 4) {
            minBps = T4_HAIRCUT_BPS_MIN;
            maxBps = T4_HAIRCUT_BPS_MAX;
        } else {
            revert InvalidTier(tier);
        }
        if (haircutBps < minBps || haircutBps > maxBps) {
            revert HaircutOutOfRange(tier, haircutBps);
        }
    }

    /// @dev USD price of `asset` (1e18-scaled): the wired oracle's quote,
    ///      or `PRICE_SCALE` ($1.00) in stub mode.
    function _priceOf(address asset) internal view returns (uint256) {
        IOracleConsumer source = oracle;
        if (address(source) == address(0)) {
            return PRICE_SCALE;
        }
        return source.priceOf(asset);
    }
}
