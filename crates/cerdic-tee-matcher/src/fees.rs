//! Volume-tiered taker fee / maker rebate schedule.
//!
//! Cerdic had no fee engine at all before this
//! (docs/trade-xyz-research.md section 7: "not carried over — Cerdic has
//! no fee engine yet"). This is a deliberately reduced version of
//! trade[XYZ]'s seven-tier, rolling-14-day schedule: four tiers here, and
//! cumulative-since-inception volume rather than a rolling window
//! (`AppState::trader_volume` never expires an entry or resets, the same
//! documented unbounded-growth limitation `last_nonce`/`portfolio_markets`/
//! `owner_addresses` already have — a real deployment needs a
//! time-windowed volume tracker, this is not it). No non-crypto "Growth
//! Mode" discount either: Cerdic has no asset-class tagging today to key
//! that off of.
//!
//! Wired in today: only the taker-side fee (`api.rs`'s `post_order`, folded
//! into `taker_total_margin`). `maker_rebate` is computed and tested here
//! but deliberately NOT yet applied to a maker's `collateral_delta`:
//! `market_maker.rs`'s inventory inference (`infer_fill_side`) attributes a
//! public `loadSealed` collateral delta to an outstanding quote by matching
//! it against `tick*qty*IMR_BPS/BPS_DENOMINATOR` within a tight absolute
//! tolerance (a real bug fixed earlier this session, see that binary's own
//! tests) — subtracting an unaccounted-for rebate from the maker's delta
//! would silently reopen that exact bug. Applying the maker side needs
//! `infer_fill_side` updated in the same change, not bolted on separately;
//! tracked as a real, explicit follow-up rather than silently done or
//! silently skipped.

pub const BPS_DENOMINATOR: u128 = 10_000;

/// `(cumulative_volume_threshold, taker_fee_bps, maker_rebate_bps)`, ascending
/// by threshold. The first entry (threshold 0) is every trader's starting tier.
const FEE_TIERS: &[(u128, u128, u128)] =
    &[(0, 10, 2), (1_000_000, 8, 3), (10_000_000, 6, 4), (100_000_000, 4, 5)];

/// The `(taker_bps, maker_rebate_bps)` pair earned by `prior_volume` —
/// volume already done BEFORE the trade being priced, never volume the
/// trade itself would add (a trader can't buy their way into a better rate
/// on the same trade that earns it).
pub fn tier_for_volume(prior_volume: u128) -> (u128, u128) {
    let mut current = FEE_TIERS[0];
    for tier in FEE_TIERS {
        if prior_volume >= tier.0 {
            current = *tier;
        } else {
            break;
        }
    }
    (current.1, current.2)
}

/// Taker fee owed on `notional`, priced at the tier `prior_volume` earned.
pub fn taker_fee(notional: u128, prior_volume: u128) -> u128 {
    let (taker_bps, _) = tier_for_volume(prior_volume);
    notional * taker_bps / BPS_DENOMINATOR
}

/// Maker rebate owed on `notional`, priced at the tier `prior_volume`
/// earned. See module doc: computed and tested, not yet wired into a real
/// maker's `collateral_delta`.
pub fn maker_rebate(notional: u128, prior_volume: u128) -> u128 {
    let (_, maker_bps) = tier_for_volume(prior_volume);
    notional * maker_bps / BPS_DENOMINATOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_volume_starts_at_the_base_tier() {
        assert_eq!(tier_for_volume(0), (10, 2));
    }

    #[test]
    fn volume_exactly_at_a_threshold_earns_that_tier() {
        assert_eq!(tier_for_volume(1_000_000), (8, 3));
    }

    #[test]
    fn volume_one_below_a_threshold_stays_in_the_prior_tier() {
        assert_eq!(tier_for_volume(999_999), (10, 2));
    }

    #[test]
    fn volume_far_past_the_top_tier_stays_pinned_to_it() {
        assert_eq!(tier_for_volume(1_000_000_000), (4, 5));
    }

    #[test]
    fn taker_fee_matches_the_base_tier_rate() {
        // 10bps of 100_000 = 100.
        assert_eq!(taker_fee(100_000, 0), 100);
    }

    #[test]
    fn taker_fee_drops_at_a_higher_tier() {
        // 4bps of 100_000 = 40, at the top tier.
        assert_eq!(taker_fee(100_000, 1_000_000_000), 40);
    }

    #[test]
    fn maker_rebate_matches_the_base_tier_rate() {
        // 2bps of 100_000 = 20.
        assert_eq!(maker_rebate(100_000, 0), 20);
    }

    #[test]
    fn maker_rebate_rises_at_a_higher_tier() {
        // 5bps of 100_000 = 50, at the top tier — rebates get richer as
        // taker fees get cheaper, matching trade[XYZ]'s own shape.
        assert_eq!(maker_rebate(100_000, 1_000_000_000), 50);
    }

    #[test]
    fn zero_notional_never_charges_or_rebates_anything() {
        assert_eq!(taker_fee(0, 0), 0);
        assert_eq!(maker_rebate(0, 1_000_000_000), 0);
    }
}
