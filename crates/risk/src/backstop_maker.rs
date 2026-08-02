//! The kernel-owned synthetic backstop maker: a resting quote in the shared
//! CLOB, priced off the mark oracle, guaranteeing a fillable two-sided
//! price on a market before real makers have built depth on it.
//!
//! Implemented from `simulations/backstop-maker/` (three Python models, run
//! before this code was written, not after): the naive version (spot-priced,
//! no cap, no staleness check) is exactly the vulnerability class GMX's V1
//! exploit actually hit, `latestAnswer()` with no TWAP smoothing and no
//! price-impact bound. The three guards below are what turned that into a
//! design that's profitable and catches ~99.9% of manipulation attempts
//! while still serving ~99.3% of ordinary flow, per `backstop_maker_sim.py`:
//!
//! 1. **TWAP pricing** ([`fair_value`]): the quote centers on a trailing
//!    average, not the latest print, so a one-tick manipulation barely
//!    moves it. The same principle as Compound's `UniswapAnchoredView`
//!    validating a Chainlink print against a Uniswap TWAP anchor, an
//!    audited pattern, not a novel one.
//! 2. **Notional cap** (`BackstopMakerConfig::notional_cap`): bounds the
//!    worst case any single fill can hit this maker for, independent of
//!    the other two guards.
//! 3. **Last-look** ([`evaluate_fill`]): re-validates the live oracle print
//!    against a SEPARATE, faster reference at the moment of match, before
//!    accepting. Deliberately not the same window `fair_value` uses for
//!    quoting: checking against the slow window conflates ordinary
//!    multi-tick trend drift with a genuine one-tick spike, which is
//!    exactly the bug the first simulation pass found (32% of legitimate
//!    flow rejected, not just the attacker) before the fast/slow split
//!    fixed it.
//!
//! `simulations/backstop-maker/dmm_stipend_sim.py` additionally found that
//! even the fully-guarded maker is not free: ordinary informed order flow
//! (not just an adversarial attacker) costs it money on net. [`PnlLedger`]
//! makes that cost explicit and trackable rather than an unbudgeted drain
//! on whatever capital backs it, the same break-even-subsidy accounting
//! that simulation used to size a Designated-Market-Maker-style stipend.
//!
//! `SpreadModel::Dynamic` (`simulations/backstop-maker/dynamic_spread_sim.py`)
//! is Ostium's utilization-widening idea: spread grows with the maker's own
//! net inventory instead of staying flat, borrowed as a PRICING idea only,
//! not Ostium's pooled-counterparty architecture (a worse fit for a
//! portfolio-margined kernel, see the session's own research notes). It is
//! real but not free: the first pass tested it against demand that never
//! declines regardless of price, which made a wider spread look like a
//! pure win (more revenue on the exact same trades). Adding actual price
//! elasticity showed the true tradeoff, real inventory reduction (11-17%
//! lower peak exposure) bought by turning away 15-24% of demand once the
//! maker is already exposed. That's a real cost, not a rounding error, so
//! this ships `Flat` as the default and `Dynamic` as an explicit opt-in per
//! market, not a universal upgrade.

use primitive_types::U256;
use thiserror::Error;

use crate::{BPS_DENOMINATOR, SCALE};

/// Errors this module's fallible computations return.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BackstopMakerError {
    /// `fair_value`/`evaluate_fill` need at least one price in history.
    #[error("oracle history is empty")]
    EmptyHistory,
    /// An intermediate product overflowed the `U256` working precision.
    #[error("backstop maker computation overflowed")]
    Overflow,
}

/// Which side of the maker's quote a taker is hitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Taker buys, lifting the maker's ask.
    Buy,
    /// Taker sells, hitting the maker's bid.
    Sell,
}

/// How the maker's half-spread is derived. `Flat` is the default and the
/// one everything else in this module was validated against; `Dynamic` is
/// an explicit per-market opt-in, see the module docs for the real,
/// quantified tradeoff it buys (less peak inventory, less service).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadModel {
    /// `spread_bps` is a constant, independent of inventory.
    Flat,
    /// `spread_bps` is the FLOOR; the effective spread widens toward
    /// `max_spread_bps` as `|inventory| / inventory_reference` approaches
    /// 100%, linearly, capped at `max_spread_bps`.
    Dynamic {
        /// Ceiling on the effective spread, basis points.
        max_spread_bps: u128,
        /// Additional basis points added at 100% utilization.
        widen_per_unit_utilization_bps: u128,
        /// Inventory magnitude (1e18-scaled base units) counted as 100%
        /// utilized.
        inventory_reference: u128,
    },
}

/// Tunable parameters for one market's backstop maker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackstopMakerConfig {
    /// Half-spread around the TWAP center, basis points: the flat spread
    /// under [`SpreadModel::Flat`], or the floor under `Dynamic`. Mirrors
    /// [`crate::RiskMonitor::liquidity_charge`]'s per-market coefficient:
    /// the same number that prices `f_L` is the natural source for this.
    pub spread_bps: u128,
    /// How the effective spread is derived from `spread_bps` and, if
    /// `Dynamic`, the maker's current inventory.
    pub spread_model: SpreadModel,
    /// Trailing window (in oracle ticks) the quote is centered on. Slow by
    /// design, insensitive to a single-tick spike.
    pub quote_twap_window: usize,
    /// Trailing window (in oracle ticks) last-look validates against.
    /// Deliberately SHORTER than `quote_twap_window`, see the module docs.
    pub last_look_window: usize,
    /// Reject a fill if the live print deviates from the last-look
    /// reference by more than this, basis points.
    pub last_look_threshold_bps: u128,
    /// Maximum notional (1e18-scaled USD) any single fill may hit this
    /// maker for, regardless of the requested size.
    pub notional_cap: u128,
}

fn effective_spread_bps(cfg: &BackstopMakerConfig, inventory: i128) -> Result<u128, BackstopMakerError> {
    match cfg.spread_model {
        SpreadModel::Flat => Ok(cfg.spread_bps),
        SpreadModel::Dynamic { max_spread_bps, widen_per_unit_utilization_bps, inventory_reference } => {
            if inventory_reference == 0 {
                return Ok(max_spread_bps);
            }
            let utilization_bps = u128::try_from(
                U256::from(inventory.unsigned_abs()) * U256::from(BPS_DENOMINATOR)
                    / U256::from(inventory_reference),
            )
            .map_err(|_| BackstopMakerError::Overflow)?
            .min(BPS_DENOMINATOR);
            let extra = u128::try_from(
                U256::from(widen_per_unit_utilization_bps) * U256::from(utilization_bps)
                    / U256::from(BPS_DENOMINATOR),
            )
            .map_err(|_| BackstopMakerError::Overflow)?;
            Ok((cfg.spread_bps + extra).min(max_spread_bps))
        }
    }
}

/// The maker's current two-sided quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    /// Price the maker buys at (taker sells into this).
    pub bid: u128,
    /// Price the maker sells at (taker buys at this).
    pub ask: u128,
}

/// Result of [`evaluate_fill`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillDecision {
    /// The fill is accepted at `price` for `size` (`size` may be less than
    /// requested, if the notional cap bound it).
    Accepted {
        /// Execution price.
        price: u128,
        /// Executed size (base units, 1e18-scaled), capped if requested more.
        size: u128,
    },
    /// Rejected: the live oracle print moved too far from its own recent
    /// trend for this fill to be safely priced (last-look tripped).
    RejectedStalePrice,
}

fn mean(history: &[u128]) -> Result<u128, BackstopMakerError> {
    if history.is_empty() {
        return Err(BackstopMakerError::EmptyHistory);
    }
    let sum = history.iter().fold(U256::zero(), |acc, &p| acc + U256::from(p));
    let count = U256::from(history.len() as u128);
    u128::try_from(sum / count).map_err(|_| BackstopMakerError::Overflow)
}

/// The trailing average of the last `window` prices in `history` (or all of
/// `history`, if shorter than `window`). `history`'s last element is the
/// current tick's print.
fn trailing_mean(history: &[u128], window: usize) -> Result<u128, BackstopMakerError> {
    let start = history.len().saturating_sub(window);
    mean(&history[start..])
}

/// The quote centered on the SLOW (`quote_twap_window`) trailing average.
pub fn fair_value(history: &[u128], cfg: &BackstopMakerConfig) -> Result<u128, BackstopMakerError> {
    trailing_mean(history, cfg.quote_twap_window)
}

/// The maker's current two-sided [`Quote`]: `spread_bps` around
/// [`fair_value`] under [`SpreadModel::Flat`], or a spread that widens with
/// `inventory` under `Dynamic` (ignored, harmlessly, under `Flat`).
pub fn compute_quote(
    history: &[u128],
    cfg: &BackstopMakerConfig,
    inventory: i128,
) -> Result<Quote, BackstopMakerError> {
    let center = fair_value(history, cfg)?;
    let spread_bps = effective_spread_bps(cfg, inventory)?;
    let half_spread =
        u128::try_from(U256::from(center) * U256::from(spread_bps) / U256::from(BPS_DENOMINATOR))
            .map_err(|_| BackstopMakerError::Overflow)?;
    Ok(Quote { bid: center.saturating_sub(half_spread), ask: center.saturating_add(half_spread) })
}

/// Evaluates a proposed fill against all three guards: quote pricing,
/// notional cap, and last-look staleness. `history`'s last element must be
/// the live print at the moment of match. `inventory` is the maker's
/// current net position ([`PnlLedger::inventory`]), used only by
/// [`SpreadModel::Dynamic`].
///
/// Price elasticity (a taker declining a quote that's widened too far) is
/// deliberately NOT modeled here: in the real kernel that choice belongs to
/// the taker, who sees [`compute_quote`]'s price before ever submitting an
/// order, exactly like any other resting quote in the book. This function
/// only gates what the MAKER is willing to accept.
pub fn evaluate_fill(
    history: &[u128],
    cfg: &BackstopMakerConfig,
    side: Side,
    requested_size: u128,
    inventory: i128,
) -> Result<FillDecision, BackstopMakerError> {
    let quote = compute_quote(history, cfg, inventory)?;
    let price = match side {
        Side::Buy => quote.ask,
        Side::Sell => quote.bid,
    };
    let size = requested_size.min(cfg.notional_cap);

    let live_print = *history.last().ok_or(BackstopMakerError::EmptyHistory)?;
    let reference = trailing_mean(history, cfg.last_look_window)?;
    let deviation = live_print.abs_diff(reference);
    let deviation_bps =
        u128::try_from(U256::from(deviation) * U256::from(BPS_DENOMINATOR) / U256::from(reference))
            .map_err(|_| BackstopMakerError::Overflow)?;
    if deviation_bps > cfg.last_look_threshold_bps {
        return Ok(FillDecision::RejectedStalePrice);
    }

    Ok(FillDecision::Accepted { price, size })
}

/// Break-even accounting for whatever capital backs this maker
/// (`simulations/backstop-maker/dmm_stipend_sim.py`'s finding: even the
/// fully-guarded maker loses money to ordinary informed flow, not just
/// attackers, and that cost needs a named, budgeted owner rather than
/// being silently absorbed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PnlLedger {
    /// Cumulative signed PnL (1e18-scaled USD) since the ledger opened.
    pub cumulative: i128,
    /// Current net position (1e18-scaled base units, positive = long).
    /// Feeds [`SpreadModel::Dynamic`]; ignored by `Flat`.
    pub inventory: i128,
}

impl PnlLedger {
    /// Records one fill's realized result: the maker took `side` of the
    /// trade at `fill_price`, and the market subsequently marked to
    /// `mark_price`. Positive `trade_pnl` when the mark moved in the
    /// maker's favor after the fill. Also updates `inventory`: a taker Buy
    /// means the maker sold (inventory decreases), a taker Sell means the
    /// maker bought (inventory increases).
    pub fn record_fill(&mut self, side: Side, fill_price: u128, mark_price: u128, size: u128) {
        let price_diff = mark_price as i128 - fill_price as i128;
        // The maker took the OPPOSITE side of the taker: a taker Buy means
        // the maker sold, so the maker profits when mark falls below the
        // fill price, not above it.
        let signed_diff = match side {
            Side::Buy => -price_diff,
            Side::Sell => price_diff,
        };
        let trade_pnl = signed_diff.saturating_mul(size as i128) / SCALE as i128;
        self.cumulative = self.cumulative.saturating_add(trade_pnl);

        let inventory_delta = match side {
            Side::Buy => -(size as i128),
            Side::Sell => size as i128,
        };
        self.inventory = self.inventory.saturating_add(inventory_delta);
    }

    /// The break-even subsidy needed to bring cumulative PnL back to zero:
    /// zero if the maker is net profitable, otherwise the size of its
    /// net loss. This is deliberately NOT a flat stipend, only losses are
    /// reimbursed, any profit the maker made stays with it.
    pub fn breakeven_subsidy_needed(&self) -> u128 {
        if self.cumulative >= 0 {
            0
        } else {
            self.cumulative.unsigned_abs()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e18(units: u128) -> u128 {
        units * SCALE
    }

    fn default_config() -> BackstopMakerConfig {
        BackstopMakerConfig {
            spread_bps: 8,
            spread_model: SpreadModel::Flat,
            quote_twap_window: 20,
            last_look_window: 3,
            last_look_threshold_bps: 15,
            notional_cap: e18(5_000),
        }
    }

    #[test]
    fn fair_value_is_the_trailing_average() {
        let history = vec![e18(100), e18(102), e18(101), e18(103)];
        let cfg = BackstopMakerConfig { quote_twap_window: 4, ..default_config() };
        let value = fair_value(&history, &cfg).unwrap();
        assert_eq!(value, e18(100 + 102 + 101 + 103) / 4);
    }

    #[test]
    fn fair_value_uses_only_the_trailing_window_not_full_history() {
        let mut history = vec![e18(1_000)]; // far in the past, should not matter
        history.extend(vec![e18(100), e18(100), e18(100)]); // recent, within a window of 3
        let cfg = BackstopMakerConfig { quote_twap_window: 3, ..default_config() };
        let value = fair_value(&history, &cfg).unwrap();
        assert_eq!(value, e18(100), "the stale 1000 print outside the window must not pull the average");
    }

    #[test]
    fn compute_quote_centers_on_fair_value_with_correct_spread() {
        let history = vec![e18(100); 20];
        let cfg = default_config(); // 8 bps
        let quote = compute_quote(&history, &cfg, 0).unwrap();
        let half_spread = e18(100) * 8 / 10_000;
        assert_eq!(quote.ask, e18(100) + half_spread);
        assert_eq!(quote.bid, e18(100) - half_spread);
    }

    #[test]
    fn normal_drift_over_the_quoting_window_does_not_trip_last_look() {
        // A gentle, consistent drift across 20 ticks (the quoting window)
        // must NOT look like a manipulation spike to the fast 3-tick
        // last-look reference -- this is exactly the bug the first
        // simulation pass found before the fast/slow split fixed it.
        let history: Vec<u128> = (0..20).map(|i| e18(100) + e18(i) / 100).collect();
        let cfg = default_config();
        let decision = evaluate_fill(&history, &cfg, Side::Buy, e18(100), 0).unwrap();
        assert!(matches!(decision, FillDecision::Accepted { .. }), "ordinary trend drift must be served");
    }

    #[test]
    fn a_one_tick_spike_trips_last_look() {
        let mut history = vec![e18(100); 19];
        history.push(e18(100) * 103 / 100); // a 3% one-tick spike, far past the 15bps threshold
        let cfg = default_config();
        let decision = evaluate_fill(&history, &cfg, Side::Buy, e18(100), 0).unwrap();
        assert_eq!(decision, FillDecision::RejectedStalePrice);
    }

    #[test]
    fn a_small_deviation_within_threshold_is_accepted() {
        let mut history = vec![e18(100); 19];
        history.push(e18(100) * 10_010 / 10_000); // 10 bps, under the 15 bps threshold
        let cfg = default_config();
        let decision = evaluate_fill(&history, &cfg, Side::Buy, e18(100), 0).unwrap();
        assert!(matches!(decision, FillDecision::Accepted { .. }));
    }

    #[test]
    fn notional_cap_bounds_the_filled_size_not_the_request() {
        let history = vec![e18(100); 20];
        let cfg = default_config(); // cap = 5_000e18
        let decision = evaluate_fill(&history, &cfg, Side::Buy, e18(50_000), 0).unwrap();
        match decision {
            FillDecision::Accepted { size, .. } => assert_eq!(size, e18(5_000)),
            other => panic!("expected an accepted, capped fill, got {other:?}"),
        }
    }

    #[test]
    fn empty_history_is_an_explicit_error_not_a_panic() {
        let cfg = default_config();
        assert_eq!(fair_value(&[], &cfg), Err(BackstopMakerError::EmptyHistory));
        assert_eq!(evaluate_fill(&[], &cfg, Side::Buy, e18(1), 0), Err(BackstopMakerError::EmptyHistory));
    }

    #[test]
    fn pnl_ledger_credits_the_maker_when_mark_moves_in_its_favor() {
        let mut ledger = PnlLedger::default();
        // Maker sold (taker bought) at 100, mark later falls to 99: the maker sold
        // high and the asset is now worth less, a profit of $1 per unit.
        ledger.record_fill(Side::Buy, e18(100), e18(99), e18(10));
        assert_eq!(ledger.cumulative, (e18(1) as i128) * 10);
        assert_eq!(ledger.breakeven_subsidy_needed(), 0, "a maker PROFIT needs no subsidy");
    }

    #[test]
    fn pnl_ledger_reports_the_exact_loss_as_the_breakeven_subsidy() {
        let mut ledger = PnlLedger::default();
        // Maker sold (taker bought) at 100, mark later rises to 105: maker lost, informed flow.
        ledger.record_fill(Side::Buy, e18(100), e18(105), e18(10));
        let expected_loss = (e18(5) as i128) * 10;
        assert_eq!(ledger.cumulative, -expected_loss);
        assert_eq!(ledger.breakeven_subsidy_needed(), expected_loss as u128);
    }

    #[test]
    fn pnl_ledger_accumulates_across_multiple_fills() {
        let mut ledger = PnlLedger::default();
        ledger.record_fill(Side::Buy, e18(100), e18(105), e18(10)); // maker loses 50
        ledger.record_fill(Side::Sell, e18(105), e18(100), e18(10)); // maker loses 50 again
        assert_eq!(ledger.breakeven_subsidy_needed(), e18(1) * 100);
    }

    #[test]
    fn pnl_ledger_tracks_inventory_in_the_correct_direction() {
        let mut ledger = PnlLedger::default();
        ledger.record_fill(Side::Buy, e18(100), e18(100), e18(10)); // taker bought, maker sold
        assert_eq!(ledger.inventory, -(e18(10) as i128), "the maker is now net short");
        ledger.record_fill(Side::Sell, e18(100), e18(100), e18(25)); // taker sold, maker bought
        assert_eq!(ledger.inventory, e18(15) as i128, "back to net long after the larger opposite fill");
    }

    fn dynamic_config() -> BackstopMakerConfig {
        BackstopMakerConfig {
            spread_model: SpreadModel::Dynamic {
                max_spread_bps: 24,
                widen_per_unit_utilization_bps: 16,
                inventory_reference: e18(20_000),
            },
            ..default_config()
        }
    }

    #[test]
    fn dynamic_spread_matches_the_floor_at_zero_inventory() {
        let history = vec![e18(100); 20];
        let quote = compute_quote(&history, &dynamic_config(), 0).unwrap();
        let half_spread = e18(100) * 8 / 10_000; // spread_bps floor, same as Flat
        assert_eq!(quote.ask, e18(100) + half_spread);
    }

    #[test]
    fn dynamic_spread_widens_as_inventory_grows() {
        let history = vec![e18(100); 20];
        let cfg = dynamic_config();
        let half_inventory = (e18(20_000) / 2) as i128; // 50% utilization
        let quote = compute_quote(&history, &cfg, half_inventory).unwrap();
        // 8bps floor + 16bps * 50% = 16bps.
        let expected_half_spread = e18(100) * 16 / 10_000;
        assert_eq!(quote.ask, e18(100) + expected_half_spread);
    }

    #[test]
    fn dynamic_spread_is_capped_at_max_spread_bps_however_large_inventory_gets() {
        let history = vec![e18(100); 20];
        let cfg = dynamic_config();
        let huge_inventory = (e18(20_000) * 100) as i128; // 10,000% utilization, clamps to 100%
        let quote = compute_quote(&history, &cfg, huge_inventory).unwrap();
        let expected_half_spread = e18(100) * 24 / 10_000; // max_spread_bps, not blown past it
        assert_eq!(quote.ask, e18(100) + expected_half_spread);
    }

    #[test]
    fn dynamic_spread_widens_the_same_regardless_of_inventory_sign() {
        let history = vec![e18(100); 20];
        let cfg = dynamic_config();
        let long_inventory = (e18(10_000)) as i128;
        let short_inventory = -(e18(10_000) as i128);
        let long_quote = compute_quote(&history, &cfg, long_inventory).unwrap();
        let short_quote = compute_quote(&history, &cfg, short_inventory).unwrap();
        assert_eq!(long_quote, short_quote, "symmetric widening: direction doesn't matter, only magnitude");
    }

    #[test]
    fn flat_spread_model_ignores_inventory_entirely() {
        let history = vec![e18(100); 20];
        let cfg = default_config(); // SpreadModel::Flat
        let at_zero = compute_quote(&history, &cfg, 0).unwrap();
        let at_huge_inventory = compute_quote(&history, &cfg, e18(1_000_000) as i128).unwrap();
        assert_eq!(at_zero, at_huge_inventory, "Flat must not react to inventory at all");
    }
}
