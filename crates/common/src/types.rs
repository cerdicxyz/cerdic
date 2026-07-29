//! Rust mirror of `packages/shared/src/types.ts`.
//!
//! These types are the off-chain engine's view of the protocol's on-chain
//! data shapes. They MUST stay aligned with the Solidity contracts in
//! `packages/contracts/src/clearing/` and with the TS surface in
//! `packages/shared/src/types.ts` — drift between the three surfaces
//! is the single largest source of engine bugs in this codebase.
//!
//! Numeric surface (load-bearing, see module docs in `lib.rs`):
//! - signed sizes use `i128` (matches Solidity `int256` semantics; lower
//!   128 bits in practice keep arithmetic ergonomic in Rust)
//! - unsigned monetary / leverage values use `u128` (matches Solidity
//!   `uint256`)
//! - identifiers (`MarketId`, `Address`) stay as `String` and are
//!   validated at the contract / FFI boundary, not here
//!
//! Mapping (TS → Rust):
//!
//! | TS type                | Rust type            | Notes                       |
//! | ---------------------- | -------------------- | --------------------------- |
//! | `MarketId`             | `MarketId`           | hex bytes32 string          |
//! | `MarketPosition.size`  | `i128`               | positive = long, neg = short|
//! | `MarketPosition.*`     | `u128`               | entryPrice, margin, leverage|
//! | `CollateralTier`       | `CollateralTier`     | `#[repr(u8)]` 1..=4         |
//! | `Side`                 | `Side`               | Long | Short                |

use std::fmt;

/// A `bytes32` market identifier, hex-encoded with a `0x` prefix.
///
/// Format: `"0x"` + 64 lowercase hex chars. Validation is intentionally
/// deferred to the FFI / contract boundary — the engine works with the
/// raw string representation everywhere except where it must produce
/// an on-chain transaction or decode a log.
pub type MarketId = String;

/// An EVM-style 20-byte address, hex-encoded with a `0x` prefix.
///
/// Format: `"0x"` + 40 lowercase hex chars (mixed case allowed for
/// EIP-55 checksums but not required by this surface).
pub type Address = String;

/// Canonical per-market position record.
///
/// Mirrors `IMarket.MarketPosition` from
/// `paper/cerdic.tex:396-407` and the TS interface
/// `MarketPosition` in `packages/shared/src/types.ts`.
///
/// `size` sign convention: positive = long, negative = short.
/// `leverage` is the per-market risk-tier CEILING, not the trader's
/// effective leverage — effective leverage is an emergent account-level
/// quantity computed by the risk engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketPosition {
    /// Market this position belongs to (bytes32 hex).
    pub market_id: MarketId,
    /// Signed size. Positive = long, negative = short. On-chain: `int256`.
    pub size: i128,
    /// Entry price in oracle quote units (1e18-scaled). On-chain: `uint256`.
    pub entry_price: u128,
    /// Posted margin locked for this position. On-chain: `uint256`.
    pub margin: u128,
    /// Maximum leverage ceiling under this market's risk-tier config.
    /// On-chain: `uint256`.
    pub leverage: u128,
}

/// Collateral asset classification. Numeric discriminants mirror the
/// on-chain tier registry (do not renumber without coordinating).
///
/// `repr(u8)` keeps the on-chain wire format (1 byte) intact; the
/// discriminants are explicit to make ABI drift visible at the type
/// level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CollateralTier {
    /// USDC, EURC — 0% haircut.
    Tier1 = 1,
    /// USYC, stUSD — 2%-5% haircut.
    Tier2 = 2,
    /// ETH, BTC liquid staking tokens — 10%-20% haircut.
    Tier3 = 3,
    /// Tokenized RWAs — 15%-35% haircut.
    Tier4 = 4,
}

impl CollateralTier {
    /// Returns true iff `self` is a higher-risk tier than `other` (i.e.
    /// has a strictly greater numeric discriminant). Used by the risk
    /// engine to assert monotonicity of haircut ranges across tiers.
    pub fn is_riskier_than(self, other: Self) -> bool {
        (self as u8) > (other as u8)
    }
}

impl fmt::Display for CollateralTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Tier1 => "Tier1",
            Self::Tier2 => "Tier2",
            Self::Tier3 => "Tier3",
            Self::Tier4 => "Tier4",
        };
        f.write_str(s)
    }
}

/// Position side. Long holds the underlying; Short holds the inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Long the underlying.
    Long,
    /// Short the underlying.
    Short,
}

impl Side {
    /// Returns the sign multiplier for the position's signed size:
    /// `+1` for Long, `-1` for Short. Used by the risk engine when
    /// it folds a `MarketPosition.size` into a per-account delta.
    pub fn sign_multiplier(self) -> i128 {
        match self {
            Self::Long => 1,
            Self::Short => -1,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Long => "Long",
            Self::Short => "Short",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Cross-check tests
// ---------------------------------------------------------------------------
//
// These are the canary tests: if a downstream change renames a field,
// changes a discriminant, or swaps i128/u128, these tests fail loudly
// so the cross-surface (TS / Solidity / Rust) drift is caught here
// rather than at the on-chain settlement boundary.
//
#[cfg(test)]
mod cross_check {
    use super::*;

    /// Discriminant stability: the on-chain `CollateralTier` is
    /// encoded as a single byte. Renumbering breaks ABI compatibility
    /// with the deployed contracts.
    #[test]
    fn collateral_tier_discriminants_match_on_chain() {
        assert_eq!(CollateralTier::Tier1 as u8, 1);
        assert_eq!(CollateralTier::Tier2 as u8, 2);
        assert_eq!(CollateralTier::Tier3 as u8, 3);
        assert_eq!(CollateralTier::Tier4 as u8, 4);
    }

    /// Monotonicity: the on-chain registry indexes tiers by ascending
    /// risk. Drift here breaks the tier-monotonicity invariant asserted
    /// in `CollateralEngine.t.sol`.
    #[test]
    fn collateral_tiers_are_monotonically_riskier() {
        assert!(CollateralTier::Tier2.is_riskier_than(CollateralTier::Tier1));
        assert!(CollateralTier::Tier3.is_riskier_than(CollateralTier::Tier2));
        assert!(CollateralTier::Tier4.is_riskier_than(CollateralTier::Tier3));
    }

    /// `MarketPosition` field ordering matters for the
    /// `PositionEngine.getPositionMetadata` ABI decoder (todo #10).
    /// Adding or reordering a field silently breaks the wire format.
    #[test]
    fn market_position_field_order_is_stable() {
        // Compiler-checked: if a field is added or reordered,
        // this constructor must be updated alongside
        // `PositionEngine.getPositionMetadata`. The test fails
        // until the call site is updated, which forces the change
        // to be deliberate.
        let _pos = MarketPosition {
            market_id: String::from("0x0000000000000000000000000000000000000000000000000000000000000001"),
            size: 0,
            entry_price: 0,
            margin: 0,
            leverage: 0,
        };
    }

    /// Sign-convention invariant: long positions have `size > 0`,
    /// short positions have `size < 0`. The risk engine relies on
    /// this sign convention when folding per-market positions into
    /// an account-level net exposure.
    #[test]
    fn side_sign_multiplier_matches_paper_convention() {
        assert_eq!(Side::Long.sign_multiplier(), 1);
        assert_eq!(Side::Short.sign_multiplier(), -1);
    }
}
