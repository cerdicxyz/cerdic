//! The kernel-owned backstop maker: a resting quote consulted only after
//! `OrderBook::submit` has exhausted real resting liquidity, guaranteeing a
//! fillable price on a market before real makers have built depth on it.
//!
//! This is a `u64`-tick-native port of `crates/risk::backstop_maker`'s
//! validated algorithm (TWAP pricing, fast/slow last-look, notional cap,
//! optional Ostium-style dynamic spread), not a direct call into that
//! crate: `book.rs`'s matching engine runs raw `u64` `tick`/`qty` with no
//! `1e18` scale correction anywhere (`required_margin = tick * qty *
//! IMR_BPS / BPS_DENOMINATOR`, api.rs), while `crates::risk` assumes
//! `1e18`-scaled `u128` throughout, matching `RiskMonitor.sol`'s
//! convention. Those are two different fixed-point conventions and
//! nothing in this crate pins down what a `tick` represents in real
//! units yet (still MVP-scoped, no market-registration config exists to
//! read that from). Force-fitting one scale into the other here is
//! exactly the kind of silent unit-conversion bug that produces wrong
//! quote/margin numbers in a LIVE matching path, so this duplicates the
//! algorithm natively in `u64` space instead of guessing at a conversion.
//! Once a real tick-size convention is pinned down, this can likely be
//! replaced with a thin wrapper around `crates::risk::backstop_maker`.
//!
//! Fed by this market's OWN realized trade prices via [`BackstopState::record_price`]
//! (the same prices already flowing into `market_data::TradeTape`), since
//! no external oracle RPC client is wired into this crate yet, see
//! `api.rs`'s module docs on the identical gap for `/liquidation-check`.
//! This is the same honest stand-in, not a fabricated feed, and inherits
//! the same limitation: a market with zero trades ever has no price to
//! quote from, [`BackstopState::try_fill`] returns `None` until at least
//! one real trade has happened.
//!
//! Findings behind the three guards: `simulations/backstop-maker/`'s
//! Python models, run before this code was written. Naive (spot-priced,
//! no cap, no staleness check) lost a mean of $56k per simulated run to a
//! one-tick manipulation attacker, exactly the vulnerability class GMX's
//! V1 exploit actually hit. Fully guarded is profitable and catches
//! ~99.9% of attacks while still serving ~99.3% of ordinary flow. The
//! optional dynamic spread (Ostium's utilization-widening idea, pricing
//! only, not its pooled-counterparty architecture) buys real inventory
//! reduction (11-17% lower peak exposure) at a real fill-rate cost
//! (15-24% of demand turned away), not a free upgrade, hence `Flat` stays
//! the default here too.

use std::collections::VecDeque;

use common::types::Side;

use crate::book::{OwnerId, Qty, Tick};

/// A well-known, pre-registered settlement identity for the backstop
/// maker, distinct from any real trader's truncated address
/// (`api.rs::signer_owner_id` never produces `u64::MAX`, it's the low 8
/// bytes of a real 20-byte address, whose top bit space is vanishingly
/// unlikely to collide but never exactly `u64::MAX` by construction of a
/// real market: this is a deliberate, reserved sentinel). Fills against
/// the backstop settle through the exact same maker-leg/proof pipeline as
/// any other fill, no new settlement path.
pub const BACKSTOP_OWNER_ID: OwnerId = u64::MAX;

/// Trailing prints kept per market. Comfortably above any configured
/// TWAP/last-look window; old entries beyond this are simply irrelevant,
/// not incorrect to drop.
const MAX_HISTORY: usize = 64;

const BPS_DENOMINATOR: u128 = 10_000;

/// How the maker's half-spread is derived. `Flat` is the default and the
/// one everything else here was validated against; `Dynamic` is an
/// explicit opt-in, see the module docs for the real, quantified tradeoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadModel {
    /// `spread_bps` is a constant, independent of inventory.
    Flat,
    /// `spread_bps` is the FLOOR; the effective spread widens toward
    /// `max_spread_bps` as `|inventory| / inventory_reference` approaches
    /// 100%, linearly, capped at `max_spread_bps`.
    Dynamic {
        /// Ceiling on the effective spread, basis points.
        max_spread_bps: u64,
        /// Additional basis points added at 100% utilization.
        widen_per_unit_utilization_bps: u64,
        /// Inventory magnitude (raw `Qty` units) counted as 100% utilized.
        inventory_reference: u64,
    },
}

/// Tunable parameters for one market's backstop maker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackstopConfig {
    /// Half-spread around the TWAP center, basis points: the flat spread
    /// under [`SpreadModel::Flat`], or the floor under `Dynamic`.
    pub spread_bps: u64,
    /// How the effective spread is derived from `spread_bps` and, if
    /// `Dynamic`, the maker's current inventory.
    pub spread_model: SpreadModel,
    /// Trailing window (in recorded prints) the quote is centered on.
    /// Slow by design, insensitive to a single-tick spike.
    pub quote_twap_window: usize,
    /// Trailing window (in recorded prints) last-look validates against.
    /// Deliberately SHORTER than `quote_twap_window`: checking the live
    /// print against the same slow window conflates ordinary multi-tick
    /// trend drift with a genuine one-tick spike (the exact bug the first
    /// `crates::risk::backstop_maker` simulation pass found before the
    /// fast/slow split fixed it).
    pub last_look_window: usize,
    /// Reject a fill if the live print deviates from the last-look
    /// reference by more than this, basis points.
    pub last_look_threshold_bps: u64,
    /// Maximum quantity any single fill may hit this maker for,
    /// regardless of the requested size.
    pub notional_cap: Qty,
}

impl Default for BackstopConfig {
    fn default() -> Self {
        Self {
            spread_bps: 8,
            spread_model: SpreadModel::Flat,
            quote_twap_window: 20,
            last_look_window: 3,
            last_look_threshold_bps: 15,
            notional_cap: Qty::MAX,
        }
    }
}

/// Per-market backstop maker state: recent price history, current
/// inventory, and cumulative realized PnL (the break-even-subsidy
/// accounting `simulations/backstop-maker/dmm_stipend_sim.py` found is
/// necessary: even the fully-guarded maker loses money to ordinary
/// informed flow, not just attackers, and that cost needs a named,
/// trackable owner rather than being silently absorbed).
#[derive(Debug, Default)]
pub struct BackstopState {
    history: VecDeque<Tick>,
    /// Current net position (positive = long). Feeds [`SpreadModel::Dynamic`].
    pub inventory: i64,
    /// Cumulative signed PnL. Not yet marked to a live price on every
    /// tick (no oracle client, see module docs), only updated at fill
    /// time against the next recorded print, same convention as the
    /// simulations this was validated against.
    pub cumulative_pnl: i128,
}

impl BackstopState {
    /// Records a newly realized trade price for this market.
    pub fn record_price(&mut self, price: Tick) {
        if let Some(&previous_last) = self.history.back() {
            self.mark_pnl_to(previous_last, price);
        }
        self.history.push_back(price);
        if self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }
    }

    /// Marks any open inventory from `from_price` to `to_price`: the same
    /// realized-PnL convention `risk::backstop_maker::PnlLedger` uses,
    /// applied here each time a fresh print arrives rather than deferred.
    fn mark_pnl_to(&mut self, from_price: Tick, to_price: Tick) {
        if self.inventory == 0 {
            return;
        }
        let price_delta = to_price as i128 - from_price as i128;
        self.cumulative_pnl =
            self.cumulative_pnl.saturating_add(price_delta.saturating_mul(self.inventory as i128));
    }

    fn trailing_mean(&self, window: usize) -> Option<Tick> {
        if self.history.is_empty() {
            return None;
        }
        let start = self.history.len().saturating_sub(window);
        let count = (self.history.len() - start) as u128;
        let sum: u128 = self.history.iter().skip(start).map(|&p| p as u128).sum();
        Some((sum / count) as Tick)
    }

    fn effective_spread_bps(&self, cfg: &BackstopConfig) -> u64 {
        match cfg.spread_model {
            SpreadModel::Flat => cfg.spread_bps,
            SpreadModel::Dynamic { max_spread_bps, widen_per_unit_utilization_bps, inventory_reference } => {
                if inventory_reference == 0 {
                    return max_spread_bps;
                }
                let utilization_bps = ((self.inventory.unsigned_abs() as u128 * BPS_DENOMINATOR)
                    / inventory_reference as u128)
                    .min(BPS_DENOMINATOR) as u64;
                let extra = (widen_per_unit_utilization_bps as u128 * utilization_bps as u128
                    / BPS_DENOMINATOR) as u64;
                (cfg.spread_bps + extra).min(max_spread_bps)
            }
        }
    }

    /// The maker's current quote price for `side` (the TAKER's side: a
    /// taker `Long` order lifts the maker's ask, `Short` hits the bid).
    /// `None` if no price has ever been recorded (a market with zero
    /// trades has nothing to quote from, see module docs).
    pub fn quote(&self, cfg: &BackstopConfig, side: Side) -> Option<Tick> {
        let center = self.trailing_mean(cfg.quote_twap_window)?;
        let spread_bps = self.effective_spread_bps(cfg);
        let half_spread = (center as u128 * spread_bps as u128 / BPS_DENOMINATOR) as u64;
        Some(match side {
            Side::Long => center.saturating_add(half_spread),
            Side::Short => center.saturating_sub(half_spread),
        })
    }

    /// Evaluates all three guards (TWAP pricing via [`quote`](Self::quote),
    /// notional cap, last-look staleness) and, if accepted, updates
    /// `inventory`. Returns `None` on rejection (last-look tripped, or no
    /// price history yet) rather than a zero-size accept, so callers don't
    /// need to distinguish "filled nothing" from "rejected."
    pub fn try_fill(&mut self, cfg: &BackstopConfig, side: Side, requested_qty: Qty) -> Option<(Tick, Qty)> {
        let price = self.quote(cfg, side)?;

        let live_print = *self.history.back()?;
        let reference = self.trailing_mean(cfg.last_look_window)?;
        if reference == 0 {
            return None;
        }
        let deviation_bps =
            (live_print.abs_diff(reference) as u128 * BPS_DENOMINATOR / reference as u128) as u64;
        if deviation_bps > cfg.last_look_threshold_bps {
            return None;
        }

        let qty = requested_qty.min(cfg.notional_cap);
        if qty == 0 {
            return None;
        }

        self.inventory = match side {
            Side::Long => self.inventory.saturating_sub(qty as i64),
            Side::Short => self.inventory.saturating_add(qty as i64),
        };

        Some((price, qty))
    }

    /// The break-even subsidy needed to bring cumulative PnL back to
    /// zero: zero if net profitable, otherwise the size of the net loss.
    /// Deliberately not a flat stipend, only losses are reimbursed.
    pub fn breakeven_subsidy_needed(&self) -> u128 {
        if self.cumulative_pnl >= 0 {
            0
        } else {
            self.cumulative_pnl.unsigned_abs()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(state: &mut BackstopState, prices: &[Tick]) {
        for &p in prices {
            state.record_price(p);
        }
    }

    #[test]
    fn no_history_means_no_quote() {
        let state = BackstopState::default();
        assert_eq!(state.quote(&BackstopConfig::default(), Side::Long), None);
    }

    #[test]
    fn quote_centers_on_the_trailing_average_with_correct_spread() {
        let mut state = BackstopState::default();
        seed(&mut state, &[100_000; 20]); // large enough that 8bps isn't lost to integer truncation
        let cfg = BackstopConfig::default(); // 8 bps
        let ask = state.quote(&cfg, Side::Long).unwrap();
        let bid = state.quote(&cfg, Side::Short).unwrap();
        assert_eq!(ask, 100_000 + 100_000 * 8 / 10_000);
        assert_eq!(bid, 100_000 - 100_000 * 8 / 10_000);
        assert!(ask > bid, "the spread must actually be nonzero, not silently truncated to nothing");
    }

    #[test]
    fn normal_drift_over_the_quoting_window_does_not_trip_last_look() {
        let mut state = BackstopState::default();
        // A gentle, consistent drift across the quoting window must NOT
        // look like a manipulation spike to the fast last-look reference.
        seed(&mut state, &(0..20).map(|i| 100 + i / 10).collect::<Vec<_>>());
        let cfg = BackstopConfig::default();
        let decision = state.try_fill(&cfg, Side::Long, 100);
        assert!(decision.is_some(), "ordinary trend drift must be served");
    }

    #[test]
    fn a_one_tick_spike_trips_last_look() {
        let mut state = BackstopState::default();
        seed(&mut state, &[100; 19]);
        state.record_price(103); // a 3% one-tick spike, far past the 15bps default threshold
        let cfg = BackstopConfig::default();
        assert_eq!(state.try_fill(&cfg, Side::Long, 100), None);
    }

    #[test]
    fn notional_cap_bounds_the_filled_size_not_the_request() {
        let mut state = BackstopState::default();
        seed(&mut state, &[100; 20]);
        let cfg = BackstopConfig { notional_cap: 5_000, ..BackstopConfig::default() };
        let (_, qty) = state.try_fill(&cfg, Side::Long, 50_000).unwrap();
        assert_eq!(qty, 5_000);
    }

    #[test]
    fn try_fill_updates_inventory_in_the_correct_direction() {
        let mut state = BackstopState::default();
        seed(&mut state, &[100; 20]);
        let cfg = BackstopConfig::default();
        state.try_fill(&cfg, Side::Long, 10).unwrap(); // taker bought, maker sold
        assert_eq!(state.inventory, -10);
        state.try_fill(&cfg, Side::Short, 25).unwrap(); // taker sold, maker bought
        assert_eq!(state.inventory, 15);
    }

    #[test]
    fn dynamic_spread_widens_with_inventory_and_caps_at_max() {
        let mut state = BackstopState::default();
        seed(&mut state, &[100_000; 20]);
        let cfg = BackstopConfig {
            spread_model: SpreadModel::Dynamic {
                max_spread_bps: 24,
                widen_per_unit_utilization_bps: 16,
                inventory_reference: 20_000,
            },
            ..BackstopConfig::default()
        };
        assert_eq!(
            state.quote(&cfg, Side::Long).unwrap(),
            100_000 + 100_000 * 8 / 10_000,
            "floor at zero inventory"
        );

        state.inventory = -10_000; // 50% utilization
        assert_eq!(state.quote(&cfg, Side::Long).unwrap(), 100_000 + 100_000 * 16 / 10_000);

        state.inventory = -1_000_000; // far past 100% utilization
        assert_eq!(
            state.quote(&cfg, Side::Long).unwrap(),
            100_000 + 100_000 * 24 / 10_000,
            "capped at max_spread_bps"
        );
    }

    #[test]
    fn breakeven_subsidy_matches_realized_losses_only() {
        let mut state = BackstopState::default();
        state.record_price(100);
        state.try_fill(&BackstopConfig::default(), Side::Long, 10).unwrap(); // maker sold at ~100.08, now short 10
        state.record_price(110); // price rises: the maker's short position lost
        assert!(state.cumulative_pnl < 0, "a short position that got squeezed is a real loss");
        assert_eq!(state.breakeven_subsidy_needed(), state.cumulative_pnl.unsigned_abs());
    }

    #[test]
    fn breakeven_subsidy_is_zero_when_net_profitable() {
        let mut state = BackstopState::default();
        state.record_price(100);
        state.try_fill(&BackstopConfig::default(), Side::Long, 10).unwrap(); // maker sold, now short 10
        state.record_price(90); // price falls: the maker's short position profited
        assert!(state.cumulative_pnl > 0);
        assert_eq!(state.breakeven_subsidy_needed(), 0);
    }
}
