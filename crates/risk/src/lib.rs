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
//! `RiskMonitor.sol`'s on-chain `currentMarginRequirement` stays ISOLATED
//! margin only (the sum of per-market maintenance requirements, no
//! cross-market offsets): a full on-chain recomputation of scenario sets and
//! correlation matrices is not gas-shaped work. This crate additionally
//! implements the full portfolio margin engine, `M(P) = f_S + f_C + f_L + f_K`
//! (paper/cerdic.tex `sec:margin`), computed here and enforced on-chain via a
//! TEE attestation rather than recomputed in Solidity. See
//! [`RiskMonitor::compute_portfolio_margin`].
//!
//! Isolated margin, MMR-based not IMR-based: `Σ |positionSize| · markPrice · MMR_BPS / 1e4`
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

/// A parallel/skewed price-shift scenario for [`RiskMonitor::compute_portfolio_margin`]'s
/// `f_S` term (paper/cerdic.tex `sec:margin`). Each shift is basis points applied to the
/// current mark price (negative = price drop); markets absent from a scenario are held flat.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scenario {
    /// Human-readable label (e.g. "parallel -10%", "funding spike").
    pub label: String,
    /// Per-market price shift, in basis points of the current mark price.
    pub price_shifts_bps: HashMap<MarketId, i128>,
}

/// One pairwise correlation entry feeding `f_K`. `rho_bps` in `[-10_000, 10_000]`
/// (basis points of correlation, so 10_000 = ρ = 1.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelationEntry {
    /// First market in the pair.
    pub market_a: MarketId,
    /// Second market in the pair.
    pub market_b: MarketId,
    /// Correlation coefficient, basis points, signed.
    pub rho_bps: i128,
}

/// Inputs to the full portfolio margin engine, `paper/cerdic.tex:267`:
/// `M(P) = f_S(P) + f_C(P) + f_L(P) + f_K(P)`.
///
/// Computed off-chain (this crate). `RiskMonitor.sol` enforces the result via a
/// TEE attestation rather than recomputing it on-chain, since scenario sets and
/// correlation matrices are not gas-shaped work. See `sec:margin`'s
/// "off-chain computed, on-chain enforced" framing.
#[derive(Debug, Clone, Default)]
pub struct PortfolioMarginParams {
    /// Scenario set `S` for the scenario-margin term.
    pub scenarios: Vec<Scenario>,
    /// Market -> asset group label, for the concentration charge.
    pub asset_groups: HashMap<MarketId, String>,
    /// Concentration threshold `theta`, basis points of total gross notional.
    pub concentration_threshold_bps: u128,
    /// Charge rate applied to the notional over `theta`, basis points.
    pub concentration_kappa_bps: u128,
    /// Per-market liquidity coefficient `l_p`, basis points of notional.
    pub liquidity_bps: HashMap<MarketId, u128>,
    /// Pairwise correlations feeding `f_K`.
    pub correlations: Vec<CorrelationEntry>,
    /// Correlation-adjustment scaling factor `beta`, basis points.
    pub beta_bps: u128,
}

/// Breakdown of a full portfolio margin evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortfolioMarginResult {
    /// Scenario margin: worst-case loss across `S` (1e18-scaled USD).
    pub f_scenario: u128,
    /// Concentration charge (1e18-scaled USD).
    pub f_concentration: u128,
    /// Liquidity charge (1e18-scaled USD).
    pub f_liquidity: u128,
    /// Correlation adjustment: negative for a net hedge credit, positive for
    /// concentrated same-direction risk (1e18-scaled USD, signed).
    pub f_correlation: i128,
    /// `M(P)`, floored at zero: `f_S + f_C + f_L + f_K`.
    pub total: u128,
}

fn signed_exposure_usd(size: i128, mark_price: u128) -> Result<i128, RiskError> {
    let magnitude = U256::from(size.unsigned_abs()) * U256::from(mark_price) / U256::from(SCALE);
    let magnitude = i128::try_from(magnitude).map_err(|_| RiskError::RequirementOverflow)?;
    Ok(if size < 0 { -magnitude } else { magnitude })
}

impl RiskMonitor {
    /// `f_S`: worst-case loss across the scenario set, floored at zero (a
    /// scenario where the portfolio gains is simply not the binding one).
    /// Markets a scenario doesn't mention are held at their current mark price.
    pub fn scenario_margin(
        positions: &[PositionState],
        oracle_mark_prices: &HashMap<MarketId, u128>,
        scenarios: &[Scenario],
    ) -> Result<u128, RiskError> {
        let mut worst_loss: i128 = 0;
        for scenario in scenarios {
            let mut change: i128 = 0;
            for position in positions {
                if position.size == 0 {
                    continue;
                }
                let mark_price = *oracle_mark_prices
                    .get(&position.market_id)
                    .ok_or_else(|| RiskError::MissingMarkPrice(position.market_id.clone()))?;
                let shift_bps = scenario.price_shifts_bps.get(&position.market_id).copied().unwrap_or(0);
                let shifted_price = {
                    let delta = (U256::from(mark_price) * U256::from(shift_bps.unsigned_abs()))
                        / U256::from(BPS_DENOMINATOR);
                    let delta = i128::try_from(delta).map_err(|_| RiskError::RequirementOverflow)?;
                    if shift_bps < 0 {
                        mark_price as i128 - delta
                    } else {
                        mark_price as i128 + delta
                    }
                };
                let position_change = signed_exposure_usd(position.size, shifted_price.max(0) as u128)?
                    .checked_sub(signed_exposure_usd(position.size, mark_price)?)
                    .ok_or(RiskError::RequirementOverflow)?;
                change = change.checked_add(position_change).ok_or(RiskError::RequirementOverflow)?;
            }
            // Loss-oriented: a scenario that gains value isn't the binding one.
            let loss = change.saturating_neg();
            worst_loss = worst_loss.max(loss);
        }
        Ok(worst_loss.max(0) as u128)
    }

    /// `f_C`: for each asset group whose gross exposure share exceeds `theta`,
    /// charge `kappa` on the excess over `theta * total_notional`.
    pub fn concentration_charge(
        positions: &[PositionState],
        oracle_mark_prices: &HashMap<MarketId, u128>,
        asset_groups: &HashMap<MarketId, String>,
        threshold_bps: u128,
        kappa_bps: u128,
    ) -> Result<u128, RiskError> {
        let mut group_notional: HashMap<&str, u128> = HashMap::new();
        let mut total_notional: u128 = 0;
        for position in positions {
            if position.size == 0 {
                continue;
            }
            let mark_price = *oracle_mark_prices
                .get(&position.market_id)
                .ok_or_else(|| RiskError::MissingMarkPrice(position.market_id.clone()))?;
            let notional = u128::try_from(
                U256::from(position.size.unsigned_abs()) * U256::from(mark_price) / U256::from(SCALE),
            )
            .map_err(|_| RiskError::RequirementOverflow)?;
            total_notional = total_notional.checked_add(notional).ok_or(RiskError::RequirementOverflow)?;
            let group = asset_groups.get(&position.market_id).map(String::as_str).unwrap_or("ungrouped");
            *group_notional.entry(group).or_insert(0) += notional;
        }
        if total_notional == 0 {
            return Ok(0);
        }
        let threshold_notional = total_notional * threshold_bps / BPS_DENOMINATOR;
        let mut charge: u128 = 0;
        for notional in group_notional.values() {
            if *notional > threshold_notional {
                let excess = notional - threshold_notional;
                charge += excess * kappa_bps / BPS_DENOMINATOR;
            }
        }
        Ok(charge)
    }

    /// `f_L`: `Σ |size_p| · markPrice_p · l_p / 1e4`, larger for thinner markets.
    pub fn liquidity_charge(
        positions: &[PositionState],
        oracle_mark_prices: &HashMap<MarketId, u128>,
        liquidity_bps: &HashMap<MarketId, u128>,
    ) -> Result<u128, RiskError> {
        let mut charge: u128 = 0;
        for position in positions {
            if position.size == 0 {
                continue;
            }
            let mark_price = *oracle_mark_prices
                .get(&position.market_id)
                .ok_or_else(|| RiskError::MissingMarkPrice(position.market_id.clone()))?;
            let coefficient_bps = liquidity_bps.get(&position.market_id).copied().unwrap_or(0);
            let notional =
                U256::from(position.size.unsigned_abs()) * U256::from(mark_price) / U256::from(SCALE);
            let term = notional * U256::from(coefficient_bps) / U256::from(BPS_DENOMINATOR);
            charge += u128::try_from(term).map_err(|_| RiskError::RequirementOverflow)?;
        }
        Ok(charge)
    }

    /// `f_K = beta * Σ_{i != j} rho_ij * s_i * s_j`, instantiated on SIGNED USD
    /// exposure rather than raw size so the term has USD dimension. Each pair's
    /// contribution is `beta * rho_ij * sign(e_i) * sign(e_j) * min(|e_i|, |e_j|)`:
    /// bounded by the smaller leg (a hedge can't credit more than the exposure
    /// it actually offsets), sign flips per the paper's convention: opposite
    /// directions on a positively-correlated pair (`s_i * s_j < 0`) credit
    /// margin, same-direction positions charge it.
    pub fn correlation_adjustment(
        positions: &[PositionState],
        oracle_mark_prices: &HashMap<MarketId, u128>,
        correlations: &[CorrelationEntry],
        beta_bps: u128,
    ) -> Result<i128, RiskError> {
        let mut exposures: HashMap<&str, i128> = HashMap::new();
        for position in positions {
            if position.size == 0 {
                continue;
            }
            let mark_price = *oracle_mark_prices
                .get(&position.market_id)
                .ok_or_else(|| RiskError::MissingMarkPrice(position.market_id.clone()))?;
            exposures.insert(position.market_id.as_str(), signed_exposure_usd(position.size, mark_price)?);
        }

        let mut total: i128 = 0;
        for entry in correlations {
            let (Some(&e_a), Some(&e_b)) =
                (exposures.get(entry.market_a.as_str()), exposures.get(entry.market_b.as_str()))
            else {
                continue;
            };
            let min_magnitude = e_a.unsigned_abs().min(e_b.unsigned_abs());
            let same_direction = (e_a >= 0) == (e_b >= 0);
            let sign: i128 = if same_direction { 1 } else { -1 };
            let term =
                U256::from(min_magnitude) * U256::from(entry.rho_bps.unsigned_abs()) * U256::from(beta_bps)
                    / (U256::from(BPS_DENOMINATOR) * U256::from(BPS_DENOMINATOR));
            let term = i128::try_from(term).map_err(|_| RiskError::RequirementOverflow)?;
            let rho_sign: i128 = if entry.rho_bps < 0 { -1 } else { 1 };
            total = total.checked_add(sign * rho_sign * term).ok_or(RiskError::RequirementOverflow)?;
        }
        Ok(total)
    }

    /// Full `M(P) = f_S + f_C + f_L + f_K`, floored at zero.
    pub fn compute_portfolio_margin(
        positions: &[PositionState],
        oracle_mark_prices: &HashMap<MarketId, u128>,
        params: &PortfolioMarginParams,
    ) -> Result<PortfolioMarginResult, RiskError> {
        let f_scenario = Self::scenario_margin(positions, oracle_mark_prices, &params.scenarios)?;
        let f_concentration = Self::concentration_charge(
            positions,
            oracle_mark_prices,
            &params.asset_groups,
            params.concentration_threshold_bps,
            params.concentration_kappa_bps,
        )?;
        let f_liquidity = Self::liquidity_charge(positions, oracle_mark_prices, &params.liquidity_bps)?;
        let f_correlation = Self::correlation_adjustment(
            positions,
            oracle_mark_prices,
            &params.correlations,
            params.beta_bps,
        )?;

        let sum: i128 = f_scenario as i128 + f_concentration as i128 + f_liquidity as i128 + f_correlation;
        Ok(PortfolioMarginResult {
            f_scenario,
            f_concentration,
            f_liquidity,
            f_correlation,
            total: sum.max(0) as u128,
        })
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

    #[test]
    fn scenario_margin_picks_the_worst_loss() {
        // 10 units long at $100. A -10% scenario loses $100; a +10%
        // scenario gains $100 and is not the binding one.
        let positions = vec![PositionState { market_id: market(1), size: e18(10) as i128 }];
        let scenarios = vec![
            Scenario { label: "down".into(), price_shifts_bps: HashMap::from([(market(1), -1_000)]) },
            Scenario { label: "up".into(), price_shifts_bps: HashMap::from([(market(1), 1_000)]) },
        ];
        let f_s =
            RiskMonitor::scenario_margin(&positions, &prices(&[(market(1), e18(100))]), &scenarios).unwrap();
        assert_eq!(f_s, e18(100));
    }

    #[test]
    fn scenario_margin_gain_only_scenarios_floor_at_zero() {
        let positions = vec![PositionState { market_id: market(1), size: e18(10) as i128 }];
        let scenarios =
            vec![Scenario { label: "up".into(), price_shifts_bps: HashMap::from([(market(1), 1_000)]) }];
        let f_s =
            RiskMonitor::scenario_margin(&positions, &prices(&[(market(1), e18(100))]), &scenarios).unwrap();
        assert_eq!(f_s, 0);
    }

    #[test]
    fn concentration_charge_only_hits_groups_over_threshold() {
        // $900 in "fx", $100 in "crypto": fx is 90% of a $1,000 book.
        let positions = vec![
            PositionState { market_id: market(1), size: e18(9) as i128 },
            PositionState { market_id: market(2), size: e18(1) as i128 },
        ];
        let groups = HashMap::from([(market(1), "fx".to_string()), (market(2), "crypto".to_string())]);
        let price_map = prices(&[(market(1), e18(100)), (market(2), e18(100))]);
        // Threshold 50%, kappa 10%: fx group is $400 over threshold ($900 - $500).
        let f_c = RiskMonitor::concentration_charge(&positions, &price_map, &groups, 5_000, 1_000).unwrap();
        assert_eq!(f_c, e18(40));
    }

    #[test]
    fn liquidity_charge_scales_with_size_and_coefficient() {
        let positions = vec![PositionState { market_id: market(1), size: e18(10) as i128 }];
        let liquidity = HashMap::from([(market(1), 500u128)]); // 5%
        let f_l =
            RiskMonitor::liquidity_charge(&positions, &prices(&[(market(1), e18(100))]), &liquidity).unwrap();
        assert_eq!(f_l, e18(50)); // 5% of $1,000 notional
    }

    #[test]
    fn correlation_adjustment_credits_a_genuine_hedge() {
        // Long 10 in market 1, short 10 in market 2, both $100, rho=+1.0, beta=1.0.
        let positions = vec![
            PositionState { market_id: market(1), size: e18(10) as i128 },
            PositionState { market_id: market(2), size: -(e18(10) as i128) },
        ];
        let price_map = prices(&[(market(1), e18(100)), (market(2), e18(100))]);
        let correlations =
            vec![CorrelationEntry { market_a: market(1), market_b: market(2), rho_bps: 10_000 }];
        let f_k = RiskMonitor::correlation_adjustment(&positions, &price_map, &correlations, 10_000).unwrap();
        assert_eq!(f_k, -(e18(1_000) as i128), "opposite-direction correlated positions credit margin");
    }

    #[test]
    fn correlation_adjustment_charges_same_direction_risk() {
        // Long 10 in BOTH markets: same-direction correlated bet, not a hedge.
        let positions = vec![
            PositionState { market_id: market(1), size: e18(10) as i128 },
            PositionState { market_id: market(2), size: e18(10) as i128 },
        ];
        let price_map = prices(&[(market(1), e18(100)), (market(2), e18(100))]);
        let correlations =
            vec![CorrelationEntry { market_a: market(1), market_b: market(2), rho_bps: 10_000 }];
        let f_k = RiskMonitor::correlation_adjustment(&positions, &price_map, &correlations, 10_000).unwrap();
        assert_eq!(f_k, e18(1_000) as i128, "same-direction correlated positions increase margin");
    }

    #[test]
    fn portfolio_margin_sums_all_four_terms() {
        let positions = vec![
            PositionState { market_id: market(1), size: e18(10) as i128 },
            PositionState { market_id: market(2), size: -(e18(10) as i128) },
        ];
        let price_map = prices(&[(market(1), e18(100)), (market(2), e18(100))]);
        let params = PortfolioMarginParams {
            scenarios: vec![Scenario {
                label: "down".into(),
                price_shifts_bps: HashMap::from([(market(1), -1_000), (market(2), 1_000)]),
            }],
            asset_groups: HashMap::new(),
            concentration_threshold_bps: 10_000,
            concentration_kappa_bps: 0,
            liquidity_bps: HashMap::from([(market(1), 100u128), (market(2), 100u128)]),
            correlations: vec![CorrelationEntry {
                market_a: market(1),
                market_b: market(2),
                rho_bps: 10_000,
            }],
            beta_bps: 10_000,
        };
        let result = RiskMonitor::compute_portfolio_margin(&positions, &price_map, &params).unwrap();
        assert_eq!(result.f_liquidity, e18(20)); // 1% of $1,000 + 1% of $1,000
        assert_eq!(result.f_correlation, -(e18(1_000) as i128)); // full hedge credit
        assert_eq!(
            result.total,
            (result.f_scenario as i128
                + result.f_concentration as i128
                + result.f_liquidity as i128
                + result.f_correlation)
                .max(0) as u128
        );
    }

    #[test]
    fn portfolio_margin_floors_total_at_zero() {
        // A hedge credit larger than everything else must not go negative.
        let positions = vec![
            PositionState { market_id: market(1), size: e18(10) as i128 },
            PositionState { market_id: market(2), size: -(e18(10) as i128) },
        ];
        let price_map = prices(&[(market(1), e18(100)), (market(2), e18(100))]);
        let params = PortfolioMarginParams {
            scenarios: vec![],
            asset_groups: HashMap::new(),
            concentration_threshold_bps: 10_000,
            concentration_kappa_bps: 0,
            liquidity_bps: HashMap::new(),
            correlations: vec![CorrelationEntry {
                market_a: market(1),
                market_b: market(2),
                rho_bps: 10_000,
            }],
            beta_bps: 10_000,
        };
        let result = RiskMonitor::compute_portfolio_margin(&positions, &price_map, &params).unwrap();
        assert_eq!(result.total, 0);
    }
}
