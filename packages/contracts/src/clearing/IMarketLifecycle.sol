// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {IMarket} from "./IMarket.sol";

/// @title IMarketLifecycle
/// @notice Lifecycle-callback surface a market extension exposes to the
///         clearing kernel (paper/cerdic.tex:549-556, `alg:hooks`). The
///         kernel invokes these hooks around position, funding,
///         liquidation, and oracle events so the extension can keep its
///         own derived state (funding indices, open interest, impact
///         TWAP windows) in lock-step with the kernel's authoritative
///         position store.
/// @dev    The hook-extensibility pattern is intentionally permissive
///         about execution semantics (paper lines 540-544): an extension
///         may run a public CLOB, an RFQ network, a batch auction, or a
///         TEE matcher — the kernel only requires that position changes
///         and cash flows settle through it.
///
///         Atomicity: hooks execute inside the kernel's settlement
///         transaction. A reverting hook reverts the entire settlement —
///         this is a feature, not an accident: an extension that cannot
///         reconcile its state blocks the trade rather than drifting
///         from the kernel. Extensions must therefore keep hook logic
///         total (no unsatisfiable requires) for trades they themselves
///         validated.
///
///         Trust model: hook targets are admin-registered kernel
///         components (`PositionEngine.registerDecoder`), so the kernel
///         does not pay for reentrancy guards on these calls — the same
///         checks-effects-interactions discipline as `Account.sol`
///         applies instead.
interface IMarketLifecycle {
    /// @notice Fired before a position opens. The extension may revert to
    ///         veto the open (e.g. open-interest cap breached, oracle
    ///         stale).
    /// @param  user   Trader whose position opens.
    /// @param  market Kernel market identifier.
    /// @param  size   Signed size being opened (positive = long).
    /// @param  price  Execution price (1e18-scaled USD).
    function beforeOpenPosition(address user, bytes32 market, int256 size, uint256 price) external;

    /// @notice Fired after a position has been written to the kernel's
    ///         store. `position` is the canonical record as stored.
    /// @param  user     Trader whose position opened.
    /// @param  market   Kernel market identifier.
    /// @param  position The stored position (see `IMarket.MarketPosition`).
    function afterOpenPosition(address user, bytes32 market, IMarket.MarketPosition calldata position) external;

    /// @notice Fired before a position closes. Reverting vetoes the close.
    /// @param  user     Trader whose position closes.
    /// @param  market   Kernel market identifier.
    /// @param  position The position about to close.
    function beforeClosePosition(address user, bytes32 market, IMarket.MarketPosition calldata position) external;

    /// @notice Fired after a position has closed and its PnL realized.
    /// @param  user   Trader whose position closed.
    /// @param  market Kernel market identifier.
    /// @param  pnl    Realized PnL of the closed position (1e18-scaled USD).
    function afterClosePosition(address user, bytes32 market, int256 pnl) external;

    /// @notice Fired before a funding settlement is applied at `rate`.
    ///         Lets the extension checkpoint its funding index ahead of
    ///         the kernel's lazy settlement (paper/cerdic.tex:419-420).
    /// @param  market Kernel market identifier.
    /// @param  rate   Funding rate being settled (1e18-scaled per the
    ///         extension's cadence convention).
    function beforeSettleFunding(bytes32 market, int256 rate) external;

    /// @notice Fired when `user`'s position enters liquidation. The
    ///         extension receives the position being liquidated so it can
    ///         unwind market-side state (open interest, insurance-fund
    ///         accounting).
    /// @param  user     Trader being liquidated.
    /// @param  market   Kernel market identifier.
    /// @param  position The position under liquidation.
    function onLiquidation(address user, bytes32 market, IMarket.MarketPosition calldata position) external;

    /// @notice Fired when the oracle updates the market's reference
    ///         price. Extensions maintaining impact TWAP or mark-price
    ///         medians (paper/cerdic.tex:1055-1063) update their windows
    ///         here.
    /// @param  market Kernel market identifier.
    /// @param  price  New oracle price (1e18-scaled USD).
    function onOracleUpdate(bytes32 market, uint256 price) external;
}
