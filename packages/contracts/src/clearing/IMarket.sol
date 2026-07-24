// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title IMarket
/// @notice Canonical market-extension interface of the Synchra clearing
///         kernel (paper/synchra.tex:396-407, `alg:market`). Every
///         financial primitive deployed against the kernel — perpetuals,
///         FX derivatives, structured products — implements this surface
///         so the kernel can value, fund, and validate its positions
///         without knowing the primitive's internals.
/// @dev    Positions are stored by the kernel as opaque bytes
///         (paper/synchra.tex:409); the market extension owns the encoding.
///         The MVP encoding is an `abi.encode` of the `MarketPosition`
///         struct below, written by `SettlementEngine.settleTrade`
///         (todo #11) and decoded by the extension's own
///         `IPositionDecoder.getMetadata` (see `PositionEngine.sol`) —
///         the two surfaces are kept ABI-compatible so the same contract
///         serves both roles.
///
///         All four functions are `view`: they are pure reads over the
///         position bytes, the oracle price, and the market's own risk
///         parameters. The lazy funding-index mutation model
///         (paper/synchra.tex:419-420) lives behind a separate
///         non-view update path in the perp extension (todo #14), not
///         behind these getters.
interface IMarket {
    /// @notice Canonical position record mirrored from
    ///         `paper/synchra.tex:396-407`.
    /// @param  marketId   Kernel-wide market identifier.
    /// @param  size       Signed position size (base units, 1e18-scaled).
    ///                    Positive = long, negative = short.
    /// @param  entryPrice Volume-weighted entry price (1e18-scaled USD).
    /// @param  margin     Margin locked against the position (1e18-scaled).
    /// @param  leverage   Per-market risk-tier leverage CEILING, not the
    ///                    trader's effective leverage (paper line 410-411).
    ///                    Effective leverage is an account-level quantity
    ///                    the risk engine derives, never a position field.
    struct MarketPosition {
        bytes32 marketId;
        int256 size;
        uint256 entryPrice;
        uint256 margin;
        uint256 leverage;
    }

    /// @notice Mark-to-oracle PnL of a position.
    /// @param  positionId  Identifier the extension uses to locate the
    ///         position (perp extension: `keccak256(trader, marketId)`).
    /// @param  oraclePrice Current oracle price (1e18-scaled USD).
    /// @return Signed PnL (1e18-scaled USD); negative = loss.
    function getPnL(bytes32 positionId, uint256 oraclePrice) external view returns (int256);

    /// @notice Funding payment accrued by a position over `period`,
    ///         computed lazily off the market's funding index
    ///         (paper/synchra.tex:419-420).
    /// @param  positionId Identifier the extension uses to locate the
    ///         position.
    /// @param  period     Funding period (blocks or seconds at the
    ///         extension's cadence convention).
    /// @return Signed funding amount (1e18-scaled USD); positive = the
    ///         position receives funding, negative = it pays.
    function getFunding(bytes32 positionId, uint256 period) external view returns (int256);

    /// @notice Validates that a new position of `size` backed by
    ///         `collateral` satisfies the market's open constraints —
    ///         initial-margin requirement and leverage ceiling
    ///         (paper/synchra.tex:417-418).
    /// @param  size       Signed size to open (base units, 1e18-scaled).
    /// @param  collateral Margin offered for the position (1e18-scaled).
    /// @return True when the position may open.
    function validateOpen(int256 size, uint256 collateral) external view returns (bool);

    /// @notice Validates that a position may close (e.g. fully flat after
    ///         the closing trade, no outstanding funding claim).
    /// @param  positionId Identifier the extension uses to locate the
    ///         position.
    /// @return True when the position may close.
    function validateClose(bytes32 positionId) external view returns (bool);
}
