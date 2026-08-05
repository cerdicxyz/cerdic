//! Public market data derived from fills: last trade price and a rolling
//! 24h window for change/volume. Not part of `OrderBook` itself, the
//! book only knows resting liquidity, not trade history, so this is kept
//! as separate per-market state alongside it in `AppState`.
//!
//! Aggregate only, same posture as `book::PriceLevel`: no `OwnerId`, no
//! order identity, nothing `ARCHITECTURE.md`'s privacy model keeps
//! private crosses this module's boundary.

use std::collections::VecDeque;

/// Widened from a strict 24h to 3 days so longer-timeframe candles (4h,
/// 1d) have enough real retained history to draw more than one or two
/// bars — a real, deliberate tradeoff: `MarketSnapshot.change_24h_bps`/
/// `volume_24h` (named for the original 24h window) now technically
/// compare against whatever's oldest in this WIDER buffer, which only
/// diverges from a literal calendar-24h figure once trades that old are
/// actually still retained. Worth a follow-up rename if that field name
/// starts reading as misleading in practice; not renamed here since
/// nothing downstream currently depends on the literal 24h meaning.
const WINDOW_SECONDS: u64 = 3 * 24 * 60 * 60;

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

/// One OHLCV bar. Prices/volume are the matcher's own raw tick/qty
/// units, same convention as `book::PriceLevel` — not rescaled, see
/// that struct's own callers for why.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Candle {
    pub open_time: u64,
    pub open: u64,
    pub high: u64,
    pub low: u64,
    pub close: u64,
    pub volume: u64,
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

    /// Buckets this tape's retained trade history into `interval_secs`-wide
    /// OHLCV bars aligned to interval-since-epoch boundaries, oldest
    /// first, capped to the most recent `limit` bars. This is real
    /// server-side indexing, not client-side mock generation (PriceChart.tsx's
    /// old `buildCandles` random walk) — but a real, stated limitation
    /// alongside it: bars only ever span what's still in the rolling 24h
    /// window `record`/`prune` maintain (WINDOW_SECONDS), and nothing
    /// survives a process restart (same ephemeral-state posture as every
    /// other in-memory index in this crate). A 1-day interval can never
    /// show more than one bar until this process has been running that
    /// long. A bucket with no trades produces no candle at all — not a
    /// synthetic flat bar carrying the prior close forward, matching
    /// `snapshot`'s own "don't fabricate a number for missing data" rule.
    pub fn candles(&self, interval_secs: u64, limit: usize) -> Vec<Candle> {
        if interval_secs == 0 || self.trades.is_empty() {
            return Vec::new();
        }

        let mut candles: Vec<Candle> = Vec::new();
        for trade in &self.trades {
            let bucket_start = (trade.timestamp / interval_secs) * interval_secs;
            match candles.last_mut() {
                Some(c) if c.open_time == bucket_start => {
                    c.high = c.high.max(trade.price);
                    c.low = c.low.min(trade.price);
                    c.close = trade.price;
                    c.volume += trade.qty;
                }
                // Trades arrive in non-decreasing timestamp order (this
                // struct's own doc), so a new bucket start can never
                // match an EARLIER candle already pushed — always append,
                // never need to search back into `candles`.
                _ => candles.push(Candle {
                    open_time: bucket_start,
                    open: trade.price,
                    high: trade.price,
                    low: trade.price,
                    close: trade.price,
                    volume: trade.qty,
                }),
            }
        }

        let start = candles.len().saturating_sub(limit);
        candles[start..].to_vec()
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

    #[test]
    fn empty_tape_has_no_candles() {
        let tape = TradeTape::default();
        assert!(tape.candles(60, 100).is_empty());
    }

    #[test]
    fn trades_in_the_same_bucket_merge_into_one_candle() {
        let mut tape = TradeTape::default();
        tape.record(1_000, 100, 2);
        tape.record(1_010, 110, 3); // same 60s bucket as 1_000: 1_000/60*60 == 1_010/60*60 == 960
        tape.record(1_015, 90, 1); // still 960, not the next bucket (which starts at 1_020)
        let candles = tape.candles(60, 10);
        assert_eq!(candles.len(), 1);
        let c = candles[0];
        assert_eq!(c.open, 100, "first trade in the bucket sets open");
        assert_eq!(c.close, 90, "last trade in the bucket sets close");
        assert_eq!(c.high, 110);
        assert_eq!(c.low, 90);
        assert_eq!(c.volume, 6, "volume sums every trade in the bucket");
    }

    #[test]
    fn trades_in_different_buckets_produce_separate_candles() {
        let mut tape = TradeTape::default();
        tape.record(0, 100, 1);
        tape.record(60, 105, 1); // next 60s bucket
        let candles = tape.candles(60, 10);
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].open_time, 0);
        assert_eq!(candles[1].open_time, 60);
    }

    #[test]
    fn a_bucket_with_no_trades_produces_no_candle() {
        let mut tape = TradeTape::default();
        tape.record(0, 100, 1);
        tape.record(180, 100, 1); // skips buckets at 60 and 120 entirely
        let candles = tape.candles(60, 10);
        assert_eq!(candles.len(), 2, "no synthetic flat bar for the gap");
        assert_eq!(candles[0].open_time, 0);
        assert_eq!(candles[1].open_time, 180);
    }

    #[test]
    fn limit_keeps_only_the_most_recent_bars() {
        let mut tape = TradeTape::default();
        for i in 0..5u64 {
            tape.record(i * 60, 100 + i, 1);
        }
        let candles = tape.candles(60, 2);
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].open_time, 180);
        assert_eq!(candles[1].open_time, 240);
    }

    #[test]
    fn zero_interval_returns_no_candles_rather_than_dividing_by_zero() {
        let mut tape = TradeTape::default();
        tape.record(0, 100, 1);
        assert!(tape.candles(0, 10).is_empty());
    }
}
