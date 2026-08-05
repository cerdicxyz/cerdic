//! Approximate FX market-hours calendar.
//!
//! FX trades on-chain 24/7 but has no live institutional pricing outside its
//! home market's hours: Sunday 5pm ET through Friday 5pm ET
//! (docs/trade-xyz-research.md section 1, and section 9's holiday-calendar
//! findings). Before this module, every keeper in this crate ran on a fixed
//! interval with no notion of "is this market's home venue open" — a real,
//! previously-flagged gap (docs/trade-xyz-research.md's "Known gaps" list).
//! This module lets a keeper tell an expected weekend gap in external
//! pricing apart from a genuine feed outage, instead of treating both the
//! same way.
//!
//! Known, deliberate limitation: "5pm ET" is approximated here as a fixed
//! 21:00 UTC boundary (correct for EST, UTC-5, roughly November-March).
//! During US daylight saving time (roughly March-November, ET is UTC-4),
//! this is off by one hour — real close is 5pm EDT = 20:00 UTC. No
//! timezone-database dependency exists in this crate today, so this is a
//! real, honest approximation, not silently pretended precision. A
//! production deployment trading FX through a DST transition should
//! replace this with a timezone-aware crate (e.g. chrono-tz) rather than
//! rely on this fixed offset.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_DAY: u64 = 86_400;

/// 1970-01-01 (unix day 0) was a Thursday. Weekday index convention here is
/// Mon=0 .. Sun=6, so day 0 must resolve to weekday 3 (Thursday):
/// `(0 + EPOCH_WEEKDAY_OFFSET) % 7 == 3` => offset 3.
const EPOCH_WEEKDAY_OFFSET: u64 = 3;

/// 21:00 UTC, approximating 5pm ET — see module doc for the DST caveat.
const FX_CLOSE_OPEN_UTC_SECONDS: u64 = 21 * 3600;

/// Whether the FX week is open at the given unix timestamp. Closed from
/// Friday `FX_CLOSE_OPEN_UTC_SECONDS` through Sunday `FX_CLOSE_OPEN_UTC_SECONDS`;
/// open every other moment, including all of Monday through Thursday.
pub fn fx_market_open_at(unix_seconds: u64) -> bool {
    let day = unix_seconds / SECONDS_PER_DAY;
    let weekday = (day + EPOCH_WEEKDAY_OFFSET) % 7; // Mon=0 .. Sun=6
    let seconds_in_day = unix_seconds % SECONDS_PER_DAY;

    match weekday {
        5 => false,                                       // Saturday: always closed
        4 => seconds_in_day < FX_CLOSE_OPEN_UTC_SECONDS,  // Friday: closes at the boundary
        6 => seconds_in_day >= FX_CLOSE_OPEN_UTC_SECONDS, // Sunday: opens at the boundary
        _ => true,                                        // Mon-Thu: always open
    }
}

/// `fx_market_open_at` evaluated at the current wall-clock time.
pub fn fx_market_open_now() -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    fx_market_open_at(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friday_before_close_is_open() {
        // 1970-01-02 (day 1) is a Friday. 00:00 UTC is well before the close.
        let ts = SECONDS_PER_DAY;
        assert!(fx_market_open_at(ts));
    }

    #[test]
    fn friday_at_close_boundary_is_closed() {
        let ts = SECONDS_PER_DAY + FX_CLOSE_OPEN_UTC_SECONDS;
        assert!(!fx_market_open_at(ts));
    }

    #[test]
    fn friday_one_second_before_close_is_open() {
        let ts = SECONDS_PER_DAY + FX_CLOSE_OPEN_UTC_SECONDS - 1;
        assert!(fx_market_open_at(ts));
    }

    #[test]
    fn saturday_is_always_closed() {
        // 1970-01-03 (day 2) is a Saturday.
        for hour in 0..24 {
            let ts = 2 * SECONDS_PER_DAY + hour * 3600;
            assert!(!fx_market_open_at(ts), "hour {hour} should be closed on Saturday");
        }
    }

    #[test]
    fn sunday_before_open_is_closed() {
        // 1970-01-04 (day 3) is a Sunday.
        let ts = 3 * SECONDS_PER_DAY + FX_CLOSE_OPEN_UTC_SECONDS - 1;
        assert!(!fx_market_open_at(ts));
    }

    #[test]
    fn sunday_at_open_boundary_is_open() {
        let ts = 3 * SECONDS_PER_DAY + FX_CLOSE_OPEN_UTC_SECONDS;
        assert!(fx_market_open_at(ts));
    }

    #[test]
    fn monday_through_thursday_are_always_open() {
        // 1970-01-05 (day 4) is a Monday; days 4-7 cover Mon-Thu.
        for day in 4..8 {
            for hour in 0..24 {
                let ts = day * SECONDS_PER_DAY + hour * 3600;
                assert!(fx_market_open_at(ts), "day {day} hour {hour} should be open");
            }
        }
    }

    #[test]
    fn fx_market_open_now_does_not_panic() {
        let _ = fx_market_open_now();
    }
}
