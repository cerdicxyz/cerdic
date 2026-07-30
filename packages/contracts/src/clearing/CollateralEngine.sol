// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {ICollateralEngine, ICollateralBalanceSource, IOracleConsumer} from "./ICollateralEngine.sol";
import {ProtocolConstants} from "../lib/ProtocolConstants.sol";

/// @title  CollateralEngine
/// @notice Four-tier collateral classification and effective-collateral valuation.
contract CollateralEngine is ICollateralEngine, ProtocolConstants {
    uint256 internal constant BPS_DENOMINATOR = 10_000;
    uint256 internal constant PRICE_SCALE = 1e18;

    address public immutable admin;

    /// @notice Zero address = stub mode, every asset prices at PRICE_SCALE ($1.00).
    IOracleConsumer public oracle;

    /// @notice Must be wired via setBalanceSource before effectiveCollateral is callable.
    ICollateralBalanceSource public balanceSource;

    /// @dev 0 means not registered; tier mapping doubles as the registration marker.
    mapping(address => uint8) internal _tiers;
    mapping(address => uint16) internal _haircutBps;

    /// @dev Registration order; the summation domain for effectiveCollateral.
    address[] internal _assets;

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    /// @param usdcAsset Registered as Tier 1 at 0% haircut; USYC registered as Tier 2.
    constructor(address adminAccount, address usdcAsset) {
        if (adminAccount == address(0)) revert ZeroAddress();
        if (usdcAsset == address(0)) revert ZeroAddress();

        admin = adminAccount;

        // forge-lint: disable-next-line(unsafe-typecast)
        _registerAsset(usdcAsset, 1, uint16(T1_HAIRCUT_BPS));
        // forge-lint: disable-next-line(unsafe-typecast)
        _registerAsset(USYC_ARC_TESTNET, 2, uint16(T2_HAIRCUT_BPS_MIN));
    }

    function tierOf(address asset) external view override returns (uint8) {
        uint8 tier = _tiers[asset];
        if (tier == 0) revert AssetNotRegistered(asset);
        return tier;
    }

    function haircutBpsOf(address asset) external view override returns (uint16) {
        if (_tiers[asset] == 0) revert AssetNotRegistered(asset);
        return _haircutBps[asset];
    }

    function oraclePriceOf(address asset) external view override returns (uint256) {
        return _priceOf(asset);
    }

    /// @dev public (not external) so effectiveCollateral calls it as an internal jump.
    function assetValueUsd(address asset, uint256 amount) public view override returns (uint256) {
        if (_tiers[asset] == 0) revert AssetNotRegistered(asset);
        return amount * (BPS_DENOMINATOR - _haircutBps[asset]) * _priceOf(asset) / (BPS_DENOMINATOR * PRICE_SCALE);
    }

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

    /// @notice Exposed so liquidation can sweep a flagged account's collateral
    ///         without a parallel registry.
    function registeredAssets() external view returns (address[] memory) {
        return _assets;
    }

    function registerAsset(address asset, uint8 tier, uint16 haircutBps) external override onlyAdmin {
        _registerAsset(asset, tier, haircutBps);
    }

    function setOracle(address oracle_) external onlyAdmin {
        oracle = IOracleConsumer(oracle_);
        emit OracleUpdated(oracle_);
    }

    function setBalanceSource(address source) external onlyAdmin {
        if (source == address(0)) revert ZeroAddress();
        balanceSource = ICollateralBalanceSource(source);
        emit BalanceSourceUpdated(source);
    }

    /// @dev Re-registration updates in place, never duplicates the asset entry.
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

    function _priceOf(address asset) internal view returns (uint256) {
        IOracleConsumer source = oracle;
        if (address(source) == address(0)) {
            return PRICE_SCALE;
        }
        return source.priceOf(asset);
    }
}
