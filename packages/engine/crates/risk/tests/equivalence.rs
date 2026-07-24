//! Cross-implementation equivalence tests (plan todo #15):
//! `RiskMonitor.sol`'s isolated-margin formulas transcribed into 256-bit
//! arithmetic are asserted bit-identical to the `risk` crate's public API
//! over 1000 fuzzed account states.
//!
//! The reference functions below are line-level transcriptions of the
//! Solidity evaluation order:
//!
//! ```solidity
//! requirement += size * oracle.markPrice(marketId) * MMR_BPS
//!     / (SCALE * BPS_DENOMINATOR);
//! ```
//!
//! (RiskMonitor.sol `currentMarginRequirement`, zero sizes skipped before
//! the price lookup) and
//!
//! ```solidity
//! if (withdrawValue > effectiveCollateral) return false;
//! return effectiveCollateral - withdrawValue >= currentMarginRequirement(trader);
//! ```
//!
//! (RiskMonitor.sol `isWithdrawSafe`). Both are computed with `U256`
//! intermediates exactly as solc's `uint256` arithmetic would.

use std::collections::HashMap;

use primitive_types::U256;
use proptest::prelude::*;
use risk::{AccountState, PositionState, RiskMonitor, BPS_DENOMINATOR, MMR_BPS, SCALE};

/// Upper bound for fuzzed sizes and prices: 1e24 raw units (1,000,000
/// units/dollars at 1e18 scaling). Keeps every intermediate far inside
/// both `uint256` (no on-chain overflow) and `u128` for the final
/// requirement, matching the domain the protocol can actually reach.
const MAX_RAW: u128 = 1_000_000_000_000_000_000_000_000; // 1e24

/// Solidity reference: `RiskMonitor.currentMarginRequirement`, summing the
/// per-position floored terms in registration order (addition is
/// commutative for the exact-integer sum, so order does not matter).
fn solidity_margin_requirement(positions: &[(i128, u128)]) -> U256 {
    positions
        .iter()
        .filter(|(size, _)| *size != 0)
        .map(|(size, price)| {
            U256::from(size.unsigned_abs()) * U256::from(*price) * U256::from(MMR_BPS)
                / (U256::from(SCALE) * U256::from(BPS_DENOMINATOR))
        })
        .fold(U256::zero(), |acc, term| acc + term)
}

/// Solidity reference: `RiskMonitor.isWithdrawSafe`.
fn solidity_is_withdraw_safe(requirement: U256, collateral: u128, withdraw_value: u128) -> bool {
    if withdraw_value > collateral {
        return false;
    }
    U256::from(collateral - withdraw_value) >= requirement
}

fn market(idx: u8) -> String {
    format!("0x{:0>64x}", idx)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Rust and the Solidity formula yield identical results for 1000
    /// fuzzed account states: the margin requirement, the
    /// maintenance-breach flag, and the withdraw-safety verdict all match
    /// bit-for-bit.
    #[test]
    fn rust_matches_solidity_for_fuzzed_account_states(
        raw_positions in prop::collection::vec(
            (0u8..4, -(MAX_RAW as i128)..=(MAX_RAW as i128)),
            0..8,
        ),
        raw_prices in prop::collection::vec(1u128..=MAX_RAW, 4),
        collateral in 0u128..=MAX_RAW,
        withdraw_value in 0u128..=MAX_RAW,
    ) {
        // Assemble the account state and a complete per-market price map.
        let positions: Vec<PositionState> = raw_positions
            .iter()
            .map(|(idx, size)| PositionState {
                market_id: market(*idx),
                size: *size,
            })
            .collect();
        let prices: HashMap<String, u128> = (0u8..4)
            .map(|idx| (market(idx), raw_prices[idx as usize]))
            .collect();
        let state = AccountState {
            positions: positions.clone(),
            effective_collateral: collateral,
        };

        // Solidity reference for the same state.
        let reference_terms: Vec<(i128, u128)> = raw_positions
            .iter()
            .map(|(idx, size)| (*size, raw_prices[*idx as usize]))
            .collect();
        let reference_requirement = solidity_margin_requirement(&reference_terms);

        // 1. current_margin_requirement matches exactly.
        let rust_requirement =
            RiskMonitor::current_margin_requirement(&positions, &prices).unwrap();
        prop_assert_eq!(U256::from(rust_requirement), reference_requirement);

        // 2. compute_margin breach flag matches `requirement > collateral`.
        let result = RiskMonitor::compute_margin(&state, &prices).unwrap();
        prop_assert_eq!(result.margin_requirement, rust_requirement);
        prop_assert_eq!(result.effective_collateral, collateral);
        prop_assert_eq!(
            result.maintenance_breached,
            reference_requirement > U256::from(collateral),
        );

        // 3. is_withdraw_safe matches the Solidity guard + comparison.
        let rust_safe =
            RiskMonitor::is_withdraw_safe(&state, &prices, withdraw_value).unwrap();
        prop_assert_eq!(
            rust_safe,
            solidity_is_withdraw_safe(reference_requirement, collateral, withdraw_value),
        );
    }
}
