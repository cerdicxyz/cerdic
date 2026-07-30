// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;

/// @title IMarket
/// @notice Interface every market extension (perps, FX, structured products)
///         implements so the kernel can value and validate positions without
///         knowing the primitive's internals.
interface IMarket {
    /// @notice size positive = long, negative = short.
    struct MarketPosition {
        bytes32 marketId;
        int256 size;
        uint256 entryPrice;
        uint256 margin;
        uint256 leverage;
    }

    /// @notice Mark-to-oracle PnL of a position. Negative = loss.
    function getPnL(bytes32 positionId, uint256 oraclePrice) external view returns (int256);

    /// @notice Funding accrued over `period`. Positive = position receives funding.
    function getFunding(bytes32 positionId, uint256 period) external view returns (int256);

    /// @notice True if a new `size` position backed by `collateral` may open.
    function validateOpen(int256 size, uint256 collateral) external view returns (bool);

    /// @notice True if the position may close.
    function validateClose(bytes32 positionId) external view returns (bool);
}
