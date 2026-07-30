//! Off-chain Rust mirror of `packages/contracts/src/clearing/RiskMonitor.sol`
//! (plan todo #15).
//!
//! The off-chain engine (CLOB, TEE RFQ matcher) pre-validates orders and
//! withdrawals against the SAME isolated-margin requirement the on-chain
//! monitor enforces, so a state the Rust mirror calls safe can never revert
//! on-chain for margin reasons. The formulas are identical by construction
//! and pinned by the cross-implementation `proptest` in
//! `tests/equivalence.rs`.
//!
//! MVP scope guardrails (mirrors the Solidity contract):
//! - ISOLATED margin only. The requirement is the sum of per-market
//!   maintenance requirements — NO cross-market portfolio offsets. The
//!   `f_S` / `f_C` / `f_L` / `f_K` portfolio components of `M(P)`
//!   (paper/cerdic.tex:446) are scope-OUT.
//! - MMR-based, not IMR-based: `Σ |positionSize| · markPrice · MMR_BPS / 1e4`
//!   with `MMR_BPS = 300` (3%, 60% of IMR per the plan decision). The
//!   initial-margin check (5%) lives at position open.
//!
//! Arithmetic surface (load-bearing): the Solidity formula forms the full
//! product `size · price · MMR_BPS` in `uint256` BEFORE a single floor
//! division by `1e18 · 1e4`. Reproducing that exactly requires 256-bit
//! intermediates — two 1e18-scaled factors already overflow `u128` (e.g.
//! 10 units at $100: `1e19 · 1e20 · 300 = 3e41 > 2^128`). All margin
//! arithmetic therefore runs in [`U256`] internally and converts to the
//! crate's public `u128` surface once, at the end, matching the notepad
//! learning #4 deferred-`U256` decision.

#![deny(missing_docs)]

use std::collections::HashMap;

use common::types::MarketId;
use primitive_types::U256;
use thiserror::Error;

/// Maintenance margin requirement in basis points: 3% of notional.
/// Mirrors `ProtocolConstants.MMR_BPS` and the `MMR_BPS` constant in
/// `RiskMonitor.sol` (the drift guard in `RiskMonitorTest` pins the
/// Solidity side; the unit tests below pin this side).
pub const MMR_BPS: u128 = 300;

/// Basis-point denominator (100.00%).
pub const BPS_DENOMINATOR: u128 = 10_000;

/// 1e18 price/size scaling shared with the on-chain position encodings.
pub const SCALE: u128 = 1_000_000_000_000_000_000;

/// Errors returned by the risk monitor's fallible computations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RiskError {
    /// A position references a market with no oracle mark price. On-chain
    /// this surfaces as an oracle revert; the mirror makes it an explicit
    /// error so the caller can skip or reject the order instead.
    #[error("missing mark price for market {0}")]
    MissingMarkPrice(MarketId),

    /// The aggregate margin requirement exceeded the `u128` public
    /// surface. Unreachable for any position set the protocol can actually
    /// hold (it implies aggregate notional above ~1e38 USD), but the
    /// conversion is checked rather than wrapped.
    #[error("margin requirement overflowed the u128 surface")]
    RequirementOverflow,
}

/// One open position as seen by the risk engine.
///
/// Mirrors the fields `PositionEngine.getPositionMetadata` returns for the
/// MMR computation — only the market and the size participate; entry
/// price, margin, and leverage do not (isolated maintenance margin is a
/// function of CURRENT mark price, not entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionState {
    /// Market the position belongs to (bytes32 hex string).
    pub market_id: MarketId,
    /// Signed size (positive = long, negative = short), 1e18-scaled base
    /// units. Only the absolute value enters the requirement.
    pub size: i128,
}

/// The account state the monitor computes over: the open position set plus
/// the account's effective collateral.
///
/// `effective_collateral` is the `C_eff` output of `CollateralEngine`
/// (1e18-scaled USD) — the on-chain monitor READS it from the collateral
/// engine rather than recomputing it, so the mirror takes it as an input;
/// keeping the collateral valuation in one place (on-chain or off-chain)
/// per call site is what keeps the two surfaces in agreement.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountState {
    /// Open positions. Empty-record markets are already filtered out by
    /// the caller, mirroring the on-chain `load(...).length == 0` skip.
    pub positions: Vec<PositionState>,
    /// Effective collateral `C_eff` (1e18-scaled USD).
    pub effective_collateral: u128,
}

/// Result of [`RiskMonitor::compute_margin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarginResult {
    /// Isolated maintenance-margin requirement `M` (1e18-scaled USD):
    /// `Σ |size| · markPrice · MMR_BPS / 1e4` over the position set.
    pub margin_requirement: u128,
    /// The effective collateral the computation ran against (echoed input).
    pub effective_collateral: u128,
    /// True when the account is under-margined (`M > C_eff`, i.e.
    /// `C_eff < M`) — the condition `RiskMonitor.checkLiquidation` uses to
    /// delegate flagging to `LiquidationEntry.checkAndFlag`.
    pub maintenance_breached: bool,
}

/// Stateless margin calculator mirroring `RiskMonitor.sol`.
pub struct RiskMonitor;

impl RiskMonitor {
    /// Isolated maintenance-margin requirement of the position set:
    /// `Σ |size| · markPrice · MMR_BPS / 1e4` (1e18-scaled USD).
    ///
    /// Zero-size positions are skipped before the price lookup, matching
    /// the Solidity `continue` (a zero-size position with a missing price
    /// is NOT an error, exactly as on-chain). The per-position term forms
    /// the full product in 256-bit arithmetic before one floor division,
    /// reproducing solc's evaluation order bit-for-bit.
    pub fn current_margin_requirement(
        positions: &[PositionState],
        oracle_mark_prices: &HashMap<MarketId, u128>,
    ) -> Result<u128, RiskError> {
        let mut requirement = U256::zero();
        for position in positions {
            if position.size == 0 {
                continue;
            }
            let mark_price = oracle_mark_prices
                .get(&position.market_id)
                .ok_or_else(|| RiskError::MissingMarkPrice(position.market_id.clone()))?;
            requirement +=
                U256::from(position.size.unsigned_abs()) * U256::from(*mark_price) * U256::from(MMR_BPS)
                    / (U256::from(SCALE) * U256::from(BPS_DENOMINATOR));
        }
        u128::try_from(requirement).map_err(|_| RiskError::RequirementOverflow)
    }

    /// Full margin evaluation of an account: the requirement, the
    /// collateral it ran against, and the maintenance-breach flag the
    /// on-chain `checkLiquidation` would report for the same state.
    pub fn compute_margin(
        account_state: &AccountState,
        oracle_mark_prices: &HashMap<MarketId, u128>,
    ) -> Result<MarginResult, RiskError> {
        let margin_requirement =
            Self::current_margin_requirement(&account_state.positions, oracle_mark_prices)?;
        Ok(MarginResult {
            margin_requirement,
            effective_collateral: account_state.effective_collateral,
            maintenance_breached: margin_requirement > account_state.effective_collateral,
        })
    }

    /// Withdraw safety, mirroring `RiskMonitor.isWithdrawSafe`:
    /// `C_eff − withdraw_value_usd ≥ margin_requirement`. A withdrawal
    /// valued above the entire effective collateral is trivially unsafe
    /// (the on-chain subtraction cannot underflow because of this guard).
    ///
    /// `withdraw_value_usd` is the haircut-adjusted USD value of the
    /// withdrawal (`CollateralEngine.assetValueUsd`); callers pass zero
    /// for unregistered assets, matching the on-chain try/catch.
    pub fn is_withdraw_safe(
        account_state: &AccountState,
        oracle_mark_prices: &HashMap<MarketId, u128>,
        withdraw_value_usd: u128,
    ) -> Result<bool, RiskError> {
        let margin_requirement =
            Self::current_margin_requirement(&account_state.positions, oracle_mark_prices)?;
        if withdraw_value_usd > account_state.effective_collateral {
            return Ok(false);
        }
        Ok(account_state.effective_collateral - withdraw_value_usd >= margin_requirement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1e18-scaled helper.
    const fn e18(units: u128) -> u128 {
        units * SCALE
    }

    fn market(id: u8) -> MarketId {
        format!("0x{:0>64}", id)
    }

    fn prices(entries: &[(MarketId, u128)]) -> HashMap<MarketId, u128> {
        entries.iter().cloned().collect()
    }

    #[test]
    fn requirement_matches_paper_example() {
        // 10 units at $100 = $1,000 notional; 3% MMR = $30.
        let positions = vec![PositionState { market_id: market(1), size: e18(10) as i128 }];
        let requirement =
            RiskMonitor::current_margin_requirement(&positions, &prices(&[(market(1), e18(100))])).unwrap();
        assert_eq!(requirement, e18(30));
    }

    #[test]
    fn requirement_sums_across_markets_with_no_offset() {
        // Isolated margin: long 10 in market 1 AND short 10 in market 2
        // pay the FULL sum — no cross-market hedging credit (scope-OUT).
        let positions = vec![
            PositionState { market_id: market(1), size: e18(10) as i128 },
            PositionState { market_id: market(2), size: -(e18(10) as i128) },
        ];
        let requirement = RiskMonitor::current_margin_requirement(
            &positions,
            &prices(&[(market(1), e18(100)), (market(2), e18(100))]),
        )
        .unwrap();
        assert_eq!(requirement, e18(60), "no portfolio offset in the MVP");
    }

    #[test]
    fn short_size_uses_absolute_value() {
        let positions = vec![PositionState { market_id: market(1), size: -(e18(10) as i128) }];
        let requirement =
            RiskMonitor::current_margin_requirement(&positions, &prices(&[(market(1), e18(100))])).unwrap();
        assert_eq!(requirement, e18(30));
    }

    #[test]
    fn empty_position_set_has_zero_requirement() {
        let requirement = RiskMonitor::current_margin_requirement(&[], &HashMap::new()).unwrap();
        assert_eq!(requirement, 0);
    }

    #[test]
    fn missing_mark_price_is_an_error() {
        let positions = vec![PositionState { market_id: market(1), size: e18(1) as i128 }];
        let err = RiskMonitor::current_margin_requirement(&positions, &HashMap::new()).unwrap_err();
        assert_eq!(err, RiskError::MissingMarkPrice(market(1)));
    }

    #[test]
    fn zero_size_position_skips_the_price_lookup() {
        // Mirrors the Solidity `size == 0` continue: a zero-size position
        // with a MISSING price is not an error and contributes nothing.
        let positions = vec![PositionState { market_id: market(1), size: 0 }];
        let requirement = RiskMonitor::current_margin_requirement(&positions, &HashMap::new()).unwrap();
        assert_eq!(requirement, 0);
    }

    #[test]
    fn compute_margin_flags_maintenance_breach() {
        // $20 collateral against a $30 requirement breaches.
        let state = AccountState {
            positions: vec![PositionState { market_id: market(1), size: e18(10) as i128 }],
            effective_collateral: e18(20),
        };
        let result = RiskMonitor::compute_margin(&state, &prices(&[(market(1), e18(100))])).unwrap();
        assert_eq!(result.margin_requirement, e18(30));
        assert_eq!(result.effective_collateral, e18(20));
        assert!(result.maintenance_breached);

        // $1,000 collateral against the same requirement is healthy.
        let healthy = AccountState { effective_collateral: e18(1_000), ..state };
        let result = RiskMonitor::compute_margin(&healthy, &prices(&[(market(1), e18(100))])).unwrap();
        assert!(!result.maintenance_breached);
    }

    #[test]
    fn withdraw_safety_boundary_is_inclusive() {
        // C_eff $1,000, requirement $30: withdrawing $970 lands exactly at
        // the requirement (safe, the check is `>=`); one wei more is not.
        let state = AccountState {
            positions: vec![PositionState { market_id: market(1), size: e18(10) as i128 }],
            effective_collateral: e18(1_000),
        };
        let price_map = prices(&[(market(1), e18(100))]);
        assert!(RiskMonitor::is_withdraw_safe(&state, &price_map, e18(970)).unwrap());
        assert!(!RiskMonitor::is_withdraw_safe(&state, &price_map, e18(970) + 1).unwrap());
        // Above the entire collateral: trivially unsafe, no underflow.
        assert!(!RiskMonitor::is_withdraw_safe(&state, &price_map, e18(1_001)).unwrap());
    }

    #[test]
    fn mmr_bps_mirrors_protocol_constants() {
        // Drift guard, Rust side: MMR is 3% and 60% of the 5% IMR (the
        // Solidity drift guard pins MMR_BPS to ProtocolConstants there).
        assert_eq!(MMR_BPS, 300);
        assert_eq!(MMR_BPS * 100 / 500, 60);
    }
}
