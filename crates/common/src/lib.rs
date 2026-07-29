//! Cross-crate type mirrors and small shared utilities for the engine.
//!
//! The types in this module are a hand-written Rust mirror of
//! `packages/shared/src/types.ts`. They are intentionally NOT generated from
//! the TS sources — the FFI-from-TS path is fragile and out of MVP scope.
//! A `proptest` cross-check in `types.rs` guards against drift between the
//! two surfaces; if a Solidity or TS type changes, the corresponding Rust
//! mirror must be updated in the same change set.
//!
//! Numeric surface (load-bearing — must match Solidity ABI):
//! - `size: i128`              (mirrors Solidity `int256`; on the wire we
//!   use the lower 256 bits; i128 keeps arithmetic ergonomic in Rust and
//!   overflows in debug mode)
//! - `entryPrice: u128`        (mirrors Solidity `uint256`)
//! - `margin: u128`            (mirrors Solidity `uint256`)
//! - `leverage: u128`          (mirrors Solidity `uint256`)
//! - `MarketId: String`        (lowercase hex bytes32, e.g. `"0xabc.."`)

#![deny(missing_docs)]

pub mod types;

#[cfg(test)]
mod tests {
    use super::types::{CollateralTier, MarketPosition, Side};

    #[test]
    fn it_works() {
        // Smoke test: the crate compiles, links, and the public types
        // are constructible. Type-specific cross-checks live in `types.rs`.
        let zero_market_id: String = format!("0x{}", "0".repeat(64));
        let pos = MarketPosition {
            market_id: zero_market_id,
            size: 1,
            entry_price: 60_000_000_000_000_000_000_000, // 60k * 1e18
            margin: 5_000_000_000_000_000_000_000,       // 5k  * 1e18
            leverage: 20,
        };
        assert_eq!(pos.size, 1);
        assert_eq!(pos.leverage, 20);
        // Side and tier are usable in match arms.
        let side = Side::Long;
        let tier = CollateralTier::Tier1;
        assert!(matches!(side, Side::Long));
        assert!(matches!(tier, CollateralTier::Tier1));
    }
}
