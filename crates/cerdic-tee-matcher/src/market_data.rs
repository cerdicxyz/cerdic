//! Public market data derived from fills: last trade price and a rolling
//! 24h window for change/volume. Not part of `OrderBook` itself, the
//! book only knows resting liquidity, not trade history, so this is kept
//! as separate per-market state alongside it in `AppState`.
//!
//! Aggregate only, same posture as `book::PriceLevel`: no `OwnerId`, no
//! order identity, nothing `ARCHITECTURE.md`'s privacy model keeps
//! private crosses this module's boundary.

use std::collections::VecDeque;

const WINDOW_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy)]
struct TradeRecord {
    timestamp: u64,
    price: u64,
    qty: u64,
}

/// One market's rolling trade history, pruned to the last 24h on every
/// insert. `VecDeque` because trades arrive in non-decreasing timestamp
/// order (the matcher's own clock, see `book::OrderBook::submit`'s `now`
/// parameter), so pruning is always a pop from the front, never a scan.
#[derive(Debug, Default)]
pub struct TradeTape {
    trades: VecDeque<TradeRecord>,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct MarketSnapshot {
    pub last_price: Option<u64>,
    pub last_trade_at: Option<u64>,
    /// Percent-in-basis-points change over the trailing 24h window.
    /// `None` when there isn't yet a trade older than the newest one to
    /// compare against (a market's first trade, or everything still
    /// inside the same instant), not zero, since "no data" and "zero
    /// change" are different facts.
    pub change_24h_bps: Option<i64>,
    pub volume_24h: u64,
}

impl TradeTape {
    pub fn record(&mut self, timestamp: u64, price: u64, qty: u64) {
        self.trades.push_back(TradeRecord { timestamp, price, qty });
        self.prune(timestamp);
    }

    fn prune(&mut self, now: u64) {
        let cutoff = now.saturating_sub(WINDOW_SECONDS);
        while self.trades.front().is_some_and(|t| t.timestamp < cutoff) {
            self.trades.pop_front();
        }
    }

    pub fn snapshot(&self) -> MarketSnapshot {
        let first = self.trades.front();
        let last = self.trades.back();
        let change_24h_bps = match (first, last) {
            (Some(f), Some(l)) if f.price > 0 && f.timestamp != l.timestamp => {
                Some(((l.price as i128 - f.price as i128) * 10_000 / f.price as i128) as i64)
            }
            _ => None,
        };
        MarketSnapshot {
            last_price: last.map(|t| t.price),
            last_trade_at: last.map(|t| t.timestamp),
            change_24h_bps,
            volume_24h: self.trades.iter().map(|t| t.qty).sum(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tape_has_no_last_price_and_zero_volume() {
        let tape = TradeTape::default();
        let snapshot = tape.snapshot();
        assert_eq!(snapshot.last_price, None);
        assert_eq!(snapshot.change_24h_bps, None);
        assert_eq!(snapshot.volume_24h, 0);
    }

    #[test]
    fn single_trade_has_a_last_price_but_no_change() {
        let mut tape = TradeTape::default();
        tape.record(1_000, 100, 5);
        let snapshot = tape.snapshot();
        assert_eq!(snapshot.last_price, Some(100));
        assert_eq!(snapshot.change_24h_bps, None, "one trade alone has nothing to compare against");
        assert_eq!(snapshot.volume_24h, 5);
    }

    #[test]
    fn change_is_measured_from_the_oldest_trade_still_in_the_window() {
        let mut tape = TradeTape::default();
        tape.record(1_000, 100, 2);
        tape.record(1_500, 110, 3);
        let snapshot = tape.snapshot();
        assert_eq!(snapshot.last_price, Some(110));
        assert_eq!(snapshot.change_24h_bps, Some(1_000), "10% up move is 1000 bps");
        assert_eq!(snapshot.volume_24h, 5);
    }

    #[test]
    fn trades_older_than_24h_are_pruned_and_excluded_from_change_and_volume() {
        let mut tape = TradeTape::default();
        tape.record(0, 100, 9); // will fall out of the window
        tape.record(WINDOW_SECONDS + 1, 200, 4);
        let snapshot = tape.snapshot();
        assert_eq!(snapshot.last_price, Some(200));
        assert_eq!(snapshot.change_24h_bps, None, "the only comparison point aged out of the window");
        assert_eq!(snapshot.volume_24h, 4, "the pruned trade's volume must not still be counted");
    }
}
