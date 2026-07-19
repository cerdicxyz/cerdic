//! Off-chain matching engine for the Synchra public CLOB market extension
//! (paper/synchra.tex:619-651, plan todo #16).
//!
//! The engine maintains one [`OrderBook`] per market: asks live in a
//! [`BTreeMap`] keyed ascending by price, bids in a mirror map keyed by
//! [`Reverse`] (descending), and each price level holds its resting orders
//! in a [`VecDeque`] so that within a level the earlier arrival matches
//! first — price-time priority (paper line 628).
//!
//! The engine is trusted for sequencing but not for correctness
//! (paper lines 629-630): every emitted [`MatchedTrade`] is serializable
//! and is independently verifiable on-chain against the signed order set
//! stored by `OrderBook.sol` (todo #17) before `SettlementBatcher`
//! (todo #18) flattens a matching tick's trades into
//! `SettlementEngine.settleTrade` calls.
//!
//! Gas-aware surface decisions mirrored from the paper:
//!
//! - **Modify-as-move** (paper line 627): [`OrderBook::modify_order`]
//!   keeps the order's identity (and therefore its recycle-fee bond and
//!   EIP-712 signature validity) intact. A same-price size change edits
//!   the order in place, preserving its queue slot; a price change moves
//!   the SAME order id to the back of the target level — never a
//!   cancel-plus-new-order.
//! - **Recycle fee** (paper line 954): orders carry an `expiry_block`.
//!   [`OrderBook::cleanup_expired`] evicts every order whose expiry block
//!   has been reached and returns their ids so the caller can claim the
//!   bonded recycle fee for removing stale state.
//!
//! Intended matching-tick driver loop (the 1-second tick of paper line
//! 646):
//!
//! ```text
//! book.cleanup_expired(current_block);      // recycle-fee hygiene
//! for order in sequenced_orders {
//!     let matches = book.process_order(order);
//!     batch.extend(matches.into_iter().map(|m| m.trade));
//! }
//! submit_batch(batch);                       // todo #18 / #20
//! ```
//!
//! Scope guardrails: this crate implements ONLY the public CLOB mode.
//! Encrypted orders (RFQ through the TEE, todos #25-#27) and any on-chain
//! matching logic are deliberately out of scope.

#![deny(missing_docs)]

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use common::types::Side;
pub use primitive_types::U256;

/// Order identifier. Uniqueness is required among LIVE orders only: the
/// engine driver assigns ids monotonically, and an id may be reused once
/// the previous order with that id has fully filled, been cancelled, or
/// been recycled.
pub type OrderId = u64;

/// Limit price, 1e18-scaled quote units — the same scaling the on-chain
/// `OrderBook.sol` stores and `SettlementEngine.settleTrade` consumes.
pub type Price = U256;

/// A resting or incoming limit order.
///
/// `size` follows the protocol-wide sign convention (positive = long,
/// negative = short) and MUST agree with `side`: a `Long` order carries a
/// positive size, a `Short` order a negative one. As the order is filled,
/// `size` is updated in place to the REMAINING signed size. `signature`
/// is the trader's EIP-712 signature over the on-chain typed data; the
/// matching engine treats it as opaque — verification happens at the
/// on-chain `OrderBook.sol` boundary (todo #17) and inside the settlement
/// batch path (todo #18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// Engine-unique identifier (see [`OrderId`]).
    pub id: OrderId,
    /// Long buys the underlying, Short sells it.
    pub side: Side,
    /// Limit price (1e18-scaled). Worst price the trader will accept.
    pub price: Price,
    /// Signed size remaining (base units, 1e18-scaled). Positive for
    /// Long, negative for Short; never zero on a live order.
    pub size: i128,
    /// First block at which the order no longer participates in matching
    /// and its recycle fee becomes claimable (paper line 954).
    pub expiry_block: u64,
    /// Opaque EIP-712 signature bytes over the on-chain order struct.
    pub signature: Vec<u8>,
}

impl Order {
    /// Remaining matchable quantity (absolute value of the signed size).
    pub fn quantity(&self) -> u128 {
        self.size.unsigned_abs()
    }

    /// Validates the internal consistency of the order: non-zero size,
    /// non-zero price, and side/size sign agreement. `process_order`
    /// rejects orders failing this check; callers wanting the reason can
    /// invoke `validate` directly.
    pub fn validate(&self) -> Result<(), ClobError> {
        if self.size == 0 {
            return Err(ClobError::ZeroSize);
        }
        if self.price.is_zero() {
            return Err(ClobError::ZeroPrice);
        }
        if !side_agrees_with_size(self.side, self.size) {
            return Err(ClobError::SideSizeSignMismatch {
                id: self.id,
                side: self.side,
                size: self.size,
            });
        }
        Ok(())
    }

    /// Rewrites the signed size so its absolute value equals `quantity`,
    /// preserving the side-implied sign.
    fn set_quantity(&mut self, quantity: u128) {
        let magnitude = i128::try_from(quantity).expect("quantity fits the i128 size surface");
        self.size = magnitude * self.side.sign_multiplier();
    }
}

/// Returns true when the signed size's sign matches the side convention.
fn side_agrees_with_size(side: Side, size: i128) -> bool {
    size != 0 && size.signum() == side.sign_multiplier()
}

/// A matched trade in the shape `SettlementEngine.settleTrade` consumes
/// (via the `SettlementBatcher` flattening of todo #18). `size` is
/// expressed from the long side and is strictly positive, mirroring the
/// on-chain convention; the long/short order ids identify the two signed
/// orders so the batcher can recover each trader's address and signature
/// from the on-chain `OrderBook.sol` storage. `premium` is zero for
/// margin-based instruments (perps, FX) and only non-zero for
/// premium-bearing instruments (paper line 419).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedTrade {
    /// Id of the order on the long (buy) side of the trade.
    pub long_order_id: OrderId,
    /// Id of the order on the short (sell) side of the trade.
    pub short_order_id: OrderId,
    /// Execution price — always the RESTING (maker) order's price under
    /// price-time priority.
    pub price: Price,
    /// Filled quantity (base units, 1e18-scaled), strictly positive.
    pub size: i128,
    /// Upfront premium for premium-bearing instruments; zero for perps.
    pub premium: U256,
}

/// One match event produced by [`OrderBook::process_order`]: the
/// settlement payload plus the bookkeeping the engine driver needs to
/// keep its own shadow state in sync (which maker filled, how much of
/// each side remains).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    /// The settlement payload for this fill.
    pub trade: MatchedTrade,
    /// Id of the resting (maker) order that was hit.
    pub maker_order_id: OrderId,
    /// Maker's remaining quantity after the fill (always non-negative —
    /// the absolute value of its signed size; zero = fully filled and
    /// evicted from the book).
    pub maker_remaining: i128,
    /// Incoming (taker) order's remaining quantity after the fill
    /// (always non-negative; zero = fully filled, positive = the
    /// remainder keeps sweeping or rests).
    pub taker_remaining: i128,
}

/// Errors returned by the book's mutating operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ClobError {
    /// No live order with the given id.
    #[error("order {0} not found in the book")]
    OrderNotFound(OrderId),

    /// A zero-sized order carries no quantity; use `cancel_order` to
    /// remove a resting order instead of modifying it to zero.
    #[error("order size must be non-zero")]
    ZeroSize,

    /// A zero price is not a usable limit (mirrors
    /// `SettlementEngine.ZeroPrice`).
    #[error("order price must be non-zero")]
    ZeroPrice,

    /// The signed size's sign disagrees with the declared side (a Long
    /// order must carry a positive size, a Short a negative one).
    #[error("order {id} side {side} disagrees with signed size {size}")]
    SideSizeSignMismatch {
        /// Order being validated.
        id: OrderId,
        /// Declared side.
        side: Side,
        /// Offending signed size.
        size: i128,
    },
}

/// Location of a live order in the book: which side's map and which
/// price level. Stored in the order index so `cancel_order` and
/// `modify_order` find the level in O(1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BookLocation {
    side: Side,
    price: Price,
}

/// Central limit order book with price-time priority matching.
///
/// Asks are keyed ascending (`BTreeMap<Price, _>`, best ask = lowest key,
/// first entry); bids are keyed descending (`BTreeMap<Reverse<Price>,
/// _>`, best bid = highest key, first entry). Within a price level,
/// orders rest in a [`VecDeque`] and the front — the earliest arrival —
/// matches first.
#[derive(Debug, Default)]
pub struct OrderBook {
    /// Sell side, ascending by price.
    asks: BTreeMap<Price, VecDeque<Order>>,
    /// Buy side, descending by price (via the `Reverse` comparator).
    bids: BTreeMap<Reverse<Price>, VecDeque<Order>>,
    /// Live-order index: id → (side, price level) for O(1) level lookup.
    index: HashMap<OrderId, BookLocation>,
}

impl OrderBook {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live (resting, unfilled) orders across both sides.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// True when no orders rest in the book.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Best (highest) resting bid price, if any.
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next().map(|&Reverse(price)| price)
    }

    /// Best (lowest) resting ask price, if any.
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// True when an order with `order_id` is live in the book.
    pub fn contains(&self, order_id: OrderId) -> bool {
        self.index.contains_key(&order_id)
    }

    /// Read-only access to a live order by id.
    pub fn order(&self, order_id: OrderId) -> Option<&Order> {
        let location = self.index.get(&order_id)?;
        let level = match location.side {
            Side::Long => self.bids.get(&Reverse(location.price))?,
            Side::Short => self.asks.get(&location.price)?,
        };
        level.iter().find(|order| order.id == order_id)
    }

    /// Ids of the orders resting at one price level, in queue (arrival)
    /// order — front of the queue first. Inspection helper for the engine
    /// driver's diagnostics and for tests asserting time priority.
    pub fn order_ids_at(&self, side: Side, price: Price) -> Vec<OrderId> {
        let level = match side {
            Side::Long => self.bids.get(&Reverse(price)),
            Side::Short => self.asks.get(&price),
        };
        level
            .map(|queue| queue.iter().map(|order| order.id).collect())
            .unwrap_or_default()
    }

    /// Runs one incoming order through price-time priority matching and
    /// returns every fill it produced, in execution order.
    ///
    /// A Long (buy) taker sweeps asks from the lowest price upward while
    /// the ask price is at or below its limit; a Short (sell) taker
    /// sweeps bids from the highest price downward while the bid price is
    /// at or above its limit. Every fill executes at the RESTING order's
    /// price, and within a level the earliest arrival fills first.
    ///
    /// Any quantity left over after the sweep rests in the book at the
    /// taker's limit price (behind orders already queued at that level).
    ///
    /// Orders that fail [`Order::validate`], or whose id collides with a
    /// live order, are rejected: they produce no fills and do not rest
    /// (the returned vector is empty). The engine driver gates INCOMING
    /// order expiry itself (the tick loop calls
    /// [`OrderBook::cleanup_expired`] between ticks; on-chain placement
    /// re-validates expiry), so this function does not re-check
    /// `expiry_block`.
    pub fn process_order(&mut self, order: Order) -> Vec<MatchResult> {
        let mut results = Vec::new();
        if order.validate().is_err() || self.contains(order.id) {
            return results;
        }

        let taker_id = order.id;
        let taker_side = order.side;
        let limit = order.price;
        let mut remaining = order.quantity();

        match taker_side {
            Side::Long => self.match_against_asks(taker_id, limit, &mut remaining, &mut results),
            Side::Short => self.match_against_bids(taker_id, limit, &mut remaining, &mut results),
        }

        if remaining > 0 {
            self.insert_resting(order, remaining);
        }
        results
    }

    /// Cancels a live order, removing it from its level and the index,
    /// and returns it (the recycle-fee refund path for issuer-initiated
    /// cancels, paper line 954).
    pub fn cancel_order(&mut self, order_id: OrderId) -> Result<Order, ClobError> {
        let location = self
            .index
            .remove(&order_id)
            .ok_or(ClobError::OrderNotFound(order_id))?;
        Ok(self
            .remove_from_level(&location, order_id)
            .expect("index and levels are kept consistent"))
    }

    /// Gas-aware modify (paper line 627): moves the order's slot rather
    /// than deleting and re-creating it.
    ///
    /// - Same price, new size: the order is edited in place and KEEPS its
    ///   queue position (time priority is preserved when only the size
    ///   changes).
    /// - New price: the SAME order id leaves its level and joins the back
    ///   of the target level — a slot move, not a cancel-plus-new-order,
    ///   so the order's identity, recycle-fee bond, and signature remain
    ///   valid. Time priority at the new level starts fresh, matching the
    ///   standard exchange rule for price amendments.
    ///
    /// `new_size` is the order's new REMAINING signed size and must agree
    /// with the order's side; a zero size is rejected (use
    /// [`OrderBook::cancel_order`] to remove the order).
    pub fn modify_order(
        &mut self,
        order_id: OrderId,
        new_price: Price,
        new_size: i128,
    ) -> Result<(), ClobError> {
        if new_size == 0 {
            return Err(ClobError::ZeroSize);
        }
        if new_price.is_zero() {
            return Err(ClobError::ZeroPrice);
        }
        let location = *self
            .index
            .get(&order_id)
            .ok_or(ClobError::OrderNotFound(order_id))?;
        if !side_agrees_with_size(location.side, new_size) {
            return Err(ClobError::SideSizeSignMismatch {
                id: order_id,
                side: location.side,
                size: new_size,
            });
        }

        if location.price == new_price {
            // In-place size edit: queue slot untouched.
            let level = match location.side {
                Side::Long => self
                    .bids
                    .get_mut(&Reverse(location.price))
                    .expect("indexed level exists"),
                Side::Short => self
                    .asks
                    .get_mut(&location.price)
                    .expect("indexed level exists"),
            };
            let order = level
                .iter_mut()
                .find(|order| order.id == order_id)
                .expect("index and levels are kept consistent");
            order.size = new_size;
            return Ok(());
        }

        // Price change: move the slot to the back of the new level.
        let mut order = self
            .remove_from_level(&location, order_id)
            .expect("index and levels are kept consistent");
        order.price = new_price;
        order.size = new_size;
        match location.side {
            Side::Long => self
                .bids
                .entry(Reverse(new_price))
                .or_default()
                .push_back(order),
            Side::Short => self.asks.entry(new_price).or_default().push_back(order),
        }
        self.index.insert(
            order_id,
            BookLocation {
                side: location.side,
                price: new_price,
            },
        );
        Ok(())
    }

    /// Evicts every order whose `expiry_block` has been reached (an order
    /// is expired at block `b` iff `b >= expiry_block`) and returns their
    /// ids — the claim list for the recycle fee of paper line 954, which
    /// compensates the participant who removes stale state during book
    /// traversal. Ids are returned ask-side first (ascending price), then
    /// bid-side (descending price).
    ///
    /// Runs in O(n) over the live orders; called once per matching tick,
    /// so the scan stays off the per-order hot path.
    pub fn cleanup_expired(&mut self, current_block: u64) -> Vec<OrderId> {
        let mut expired = Vec::new();
        for level in self.asks.values() {
            for order in level {
                if order.expiry_block <= current_block {
                    expired.push(order.id);
                }
            }
        }
        for level in self.bids.values() {
            for order in level {
                if order.expiry_block <= current_block {
                    expired.push(order.id);
                }
            }
        }
        for &order_id in &expired {
            // Every collected id came from a live order, so the cancel
            // cannot fail; ignore the returned order.
            let _ = self.cancel_order(order_id);
        }
        expired
    }

    /// Sweeps the ask side for a Long taker: fills against the lowest
    /// asks at or below `limit`, earliest arrival first, executing each
    /// fill at the resting price.
    fn match_against_asks(
        &mut self,
        taker_id: OrderId,
        limit: Price,
        remaining: &mut u128,
        results: &mut Vec<MatchResult>,
    ) {
        while *remaining > 0 {
            let Some(&level_price) = self.asks.keys().next() else {
                break;
            };
            if level_price > limit {
                break;
            }
            let level_empty = {
                let level = self
                    .asks
                    .get_mut(&level_price)
                    .expect("key read from the map exists");
                while *remaining > 0 {
                    let Some(maker) = level.front_mut() else {
                        break;
                    };
                    let maker_id = maker.id;
                    let fill = maker.quantity().min(*remaining);
                    let maker_remaining = maker.quantity() - fill;
                    *remaining -= fill;
                    if maker_remaining == 0 {
                        level.pop_front();
                        self.index.remove(&maker_id);
                    } else {
                        maker.set_quantity(maker_remaining);
                    }
                    results.push(MatchResult {
                        trade: MatchedTrade {
                            long_order_id: taker_id,
                            short_order_id: maker_id,
                            price: level_price,
                            size: fill as i128,
                            premium: U256::zero(),
                        },
                        maker_order_id: maker_id,
                        maker_remaining: maker_remaining as i128,
                        taker_remaining: *remaining as i128,
                    });
                }
                level.is_empty()
            };
            if level_empty {
                self.asks.remove(&level_price);
            }
        }
    }

    /// Sweeps the bid side for a Short taker: fills against the highest
    /// bids at or above `limit`, earliest arrival first, executing each
    /// fill at the resting price.
    fn match_against_bids(
        &mut self,
        taker_id: OrderId,
        limit: Price,
        remaining: &mut u128,
        results: &mut Vec<MatchResult>,
    ) {
        while *remaining > 0 {
            let Some(&Reverse(level_price)) = self.bids.keys().next() else {
                break;
            };
            if level_price < limit {
                break;
            }
            let level_empty = {
                let level = self
                    .bids
                    .get_mut(&Reverse(level_price))
                    .expect("key read from the map exists");
                while *remaining > 0 {
                    let Some(maker) = level.front_mut() else {
                        break;
                    };
                    let maker_id = maker.id;
                    let fill = maker.quantity().min(*remaining);
                    let maker_remaining = maker.quantity() - fill;
                    *remaining -= fill;
                    if maker_remaining == 0 {
                        level.pop_front();
                        self.index.remove(&maker_id);
                    } else {
                        maker.set_quantity(maker_remaining);
                    }
                    results.push(MatchResult {
                        trade: MatchedTrade {
                            long_order_id: maker_id,
                            short_order_id: taker_id,
                            price: level_price,
                            size: fill as i128,
                            premium: U256::zero(),
                        },
                        maker_order_id: maker_id,
                        maker_remaining: maker_remaining as i128,
                        taker_remaining: *remaining as i128,
                    });
                }
                level.is_empty()
            };
            if level_empty {
                self.bids.remove(&Reverse(level_price));
            }
        }
    }

    /// Pushes an order's unfilled remainder onto the back of its price
    /// level and registers it in the index.
    fn insert_resting(&mut self, mut order: Order, remaining: u128) {
        order.set_quantity(remaining);
        let id = order.id;
        let location = BookLocation {
            side: order.side,
            price: order.price,
        };
        match location.side {
            Side::Long => self
                .bids
                .entry(Reverse(order.price))
                .or_default()
                .push_back(order),
            Side::Short => self.asks.entry(order.price).or_default().push_back(order),
        }
        self.index.insert(id, location);
    }

    /// Removes `order_id` from the level described by `location`,
    /// dropping the level itself when it empties. Does NOT touch the
    /// index — callers own index bookkeeping.
    fn remove_from_level(&mut self, location: &BookLocation, order_id: OrderId) -> Option<Order> {
        let level = match location.side {
            Side::Long => self.bids.get_mut(&Reverse(location.price))?,
            Side::Short => self.asks.get_mut(&location.price)?,
        };
        let position = level.iter().position(|order| order.id == order_id)?;
        let removed = level.remove(position);
        let level_now_empty = level.is_empty();
        if level_now_empty {
            match location.side {
                Side::Long => {
                    self.bids.remove(&Reverse(location.price));
                }
                Side::Short => {
                    self.asks.remove(&location.price);
                }
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Price helper: tests use plain integer prices (the 1e18 scaling is
    /// irrelevant to book mechanics).
    fn px(value: u128) -> Price {
        U256::from(value)
    }

    fn order(id: OrderId, side: Side, price: u128, size: i128) -> Order {
        Order {
            id,
            side,
            price: px(price),
            size,
            expiry_block: u64::MAX,
            signature: vec![0xAA, 0xBB],
        }
    }

    /// Buy helper: `qty` is the positive quantity.
    fn buy(id: OrderId, price: u128, qty: i128) -> Order {
        order(id, Side::Long, price, qty)
    }

    /// Sell helper: `qty` is the positive quantity (stored negative).
    fn sell(id: OrderId, price: u128, qty: i128) -> Order {
        order(id, Side::Short, price, -qty)
    }

    #[test]
    fn non_crossing_order_rests_without_matching() {
        // QA failure scenario: a sell priced ABOVE the best bid must not
        // match — it rests, and no trade is emitted.
        let mut book = OrderBook::new();
        assert!(book.process_order(buy(1, 100, 5)).is_empty());
        assert!(book.process_order(sell(2, 105, 3)).is_empty());

        assert_eq!(book.len(), 2);
        assert_eq!(book.best_bid(), Some(px(100)));
        assert_eq!(book.best_ask(), Some(px(105)));
    }

    #[test]
    fn crossing_orders_clear_at_resting_price_with_conservation() {
        // QA happy scenario: sell 105×3 rests, buy 105×3 crosses — one
        // trade at 105 for the full size; both orders leave the book.
        let mut book = OrderBook::new();
        assert!(book.process_order(sell(1, 105, 3)).is_empty());

        let results = book.process_order(buy(2, 105, 3));
        assert_eq!(results.len(), 1);
        let trade = &results[0].trade;
        assert_eq!(trade.price, px(105));
        assert_eq!(trade.size, 3);
        assert_eq!(trade.long_order_id, 2);
        assert_eq!(trade.short_order_id, 1);
        assert_eq!(trade.premium, U256::zero());
        assert_eq!(results[0].maker_remaining, 0);
        assert_eq!(results[0].taker_remaining, 0);

        // Conservation: 3 bought == 3 sold, nothing left resting.
        assert!(book.is_empty());
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn buy_matches_lowest_ask_first() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 105, 1));
        book.process_order(sell(2, 103, 1));
        book.process_order(sell(3, 104, 1));

        let results = book.process_order(buy(4, 105, 1));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].trade.price, px(103), "best ask fills first");
        assert_eq!(results[0].trade.short_order_id, 2);
        assert_eq!(
            book.order_ids_at(Side::Short, px(103)),
            Vec::<OrderId>::new()
        );
    }

    #[test]
    fn earlier_order_at_same_price_fills_first() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 105, 2));
        book.process_order(sell(2, 105, 2));

        // Taker takes only 3 of the 4 resting units: order 1 (earlier)
        // fills completely, order 2 partially.
        let results = book.process_order(buy(3, 105, 3));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].maker_order_id, 1);
        assert_eq!(results[0].trade.size, 2);
        assert_eq!(results[1].maker_order_id, 2);
        assert_eq!(results[1].trade.size, 1);
        assert_eq!(
            results[1].maker_remaining, 1,
            "remaining quantity is unsigned"
        );
        assert_eq!(book.order(2).unwrap().size, -1);
    }

    #[test]
    fn partial_fill_leaves_taker_remainder_resting() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 105, 4));

        let results = book.process_order(buy(2, 105, 10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].trade.size, 4);
        assert_eq!(results[0].taker_remaining, 6);

        // The taker's leftover 6 rests as a bid at its limit price.
        assert_eq!(book.best_bid(), Some(px(105)));
        assert_eq!(book.order(2).unwrap().size, 6);
        assert_eq!(book.order_ids_at(Side::Long, px(105)), vec![2]);
    }

    #[test]
    fn taker_sweeps_multiple_levels_at_maker_prices() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 101, 2));
        book.process_order(sell(2, 102, 3));
        book.process_order(sell(3, 103, 4));

        let results = book.process_order(buy(4, 103, 6));
        assert_eq!(results.len(), 3);
        // Each level executes at ITS OWN resting price, ascending.
        assert_eq!(results[0].trade.price, px(101));
        assert_eq!(results[0].trade.size, 2);
        assert_eq!(results[1].trade.price, px(102));
        assert_eq!(results[1].trade.size, 3);
        assert_eq!(results[2].trade.price, px(103));
        assert_eq!(results[2].trade.size, 1);

        // 2+3+1 = 6 bought against 6 sold; 3 remain at the 103 ask.
        assert_eq!(book.order(3).unwrap().size, -3);
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn sell_matches_highest_bid_first() {
        let mut book = OrderBook::new();
        book.process_order(buy(1, 100, 1));
        book.process_order(buy(2, 102, 1));
        book.process_order(buy(3, 101, 1));

        let results = book.process_order(sell(4, 100, 2));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].trade.price, px(102), "best bid fills first");
        assert_eq!(results[0].trade.long_order_id, 2);
        assert_eq!(results[0].trade.short_order_id, 4);
        assert_eq!(results[1].trade.price, px(101));
        assert_eq!(results[1].trade.long_order_id, 3);
        // The 100 bid never got touched and the sell fully filled.
        assert_eq!(book.best_bid(), Some(px(100)));
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn cancel_order_removes_it_and_returns_the_order() {
        let mut book = OrderBook::new();
        book.process_order(buy(1, 100, 5));
        book.process_order(buy(2, 101, 5));

        let cancelled = book.cancel_order(1).unwrap();
        assert_eq!(cancelled.id, 1);
        assert_eq!(cancelled.size, 5);
        assert!(!book.contains(1));
        assert_eq!(book.len(), 1);

        // The level emptied by the cancel is dropped from the map: the
        // remaining bid is the only level left.
        assert_eq!(book.best_bid(), Some(px(101)));
    }

    #[test]
    fn cancel_unknown_order_returns_not_found() {
        let mut book = OrderBook::new();
        assert_eq!(book.cancel_order(42), Err(ClobError::OrderNotFound(42)));
    }

    #[test]
    fn modify_at_same_price_keeps_queue_position() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 105, 2));
        book.process_order(sell(2, 105, 2));

        book.modify_order(1, px(105), -5).unwrap();
        assert_eq!(book.order(1).unwrap().size, -5);
        // Queue order untouched: 1 still ahead of 2.
        assert_eq!(book.order_ids_at(Side::Short, px(105)), vec![1, 2]);

        // A sweep therefore still fills in the original FIFO order.
        let results = book.process_order(buy(3, 105, 6));
        assert_eq!(results[0].maker_order_id, 1);
        assert_eq!(results[0].trade.size, 5);
        assert_eq!(results[1].maker_order_id, 2);
    }

    #[test]
    fn modify_to_new_price_moves_to_back_of_new_level() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 104, 2));
        book.process_order(sell(2, 105, 2));

        // Move order 1 from 104 to 105: it must land BEHIND order 2 at
        // the new level (slot move, fresh time priority there), and the
        // 104 level must disappear.
        book.modify_order(1, px(105), -2).unwrap();
        assert_eq!(book.order_ids_at(Side::Short, px(105)), vec![2, 1]);
        assert_eq!(
            book.order_ids_at(Side::Short, px(104)),
            Vec::<OrderId>::new()
        );
        assert_eq!(book.best_ask(), Some(px(105)));
        assert_eq!(book.order(1).unwrap().price, px(105));

        // Size can change alongside the price in the same move.
        book.modify_order(1, px(103), -7).unwrap();
        assert_eq!(book.order(1).unwrap().size, -7);
        assert_eq!(book.best_ask(), Some(px(103)));
    }

    #[test]
    fn modify_unknown_order_returns_not_found() {
        let mut book = OrderBook::new();
        assert_eq!(
            book.modify_order(42, px(100), 1),
            Err(ClobError::OrderNotFound(42))
        );
    }

    #[test]
    fn modify_rejects_zero_size_and_sign_mismatch() {
        let mut book = OrderBook::new();
        book.process_order(buy(1, 100, 5));

        // Zero size means cancel — the modify path rejects it.
        assert_eq!(book.modify_order(1, px(100), 0), Err(ClobError::ZeroSize));
        // A Long order cannot take a negative size (and vice versa).
        assert_eq!(
            book.modify_order(1, px(100), -5),
            Err(ClobError::SideSizeSignMismatch {
                id: 1,
                side: Side::Long,
                size: -5,
            })
        );
        assert_eq!(book.modify_order(1, px(0), 5), Err(ClobError::ZeroPrice));
        // Rejected modifies leave the order untouched.
        assert_eq!(book.order(1).unwrap().size, 5);
        assert_eq!(book.order(1).unwrap().price, px(100));
    }

    #[test]
    fn cleanup_expired_removes_and_reports_expired_ids() {
        let mut book = OrderBook::new();
        let mut expiring = sell(1, 105, 3);
        expiring.expiry_block = 10;
        let mut expiring_later = buy(2, 100, 4);
        expiring_later.expiry_block = 20;
        let mut also_expiring = sell(3, 106, 1);
        also_expiring.expiry_block = 20;
        let live = buy(4, 99, 2); // expiry u64::MAX from the helper

        book.process_order(expiring);
        book.process_order(expiring_later);
        book.process_order(also_expiring);
        book.process_order(live);

        // At block 20 the orders with expiry 10 and 20 are stale.
        let expired = book.cleanup_expired(20);
        assert_eq!(expired, vec![1, 3, 2], "asks reported first, then bids");
        assert!(!book.contains(1));
        assert!(!book.contains(2));
        assert!(!book.contains(3));
        assert!(book.contains(4));
        assert_eq!(book.len(), 1);

        // Recycling is idempotent: a second pass at the same block finds
        // nothing, and cancelled ids stay gone.
        assert!(book.cleanup_expired(20).is_empty());
        assert_eq!(book.cancel_order(1), Err(ClobError::OrderNotFound(1)));
    }

    #[test]
    fn invalid_orders_produce_no_matches_and_do_not_rest() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 105, 5));

        // Zero size, zero price, and side/size sign mismatches are all
        // rejected silently (no fills, no resting).
        let zero_size = Order {
            size: 0,
            ..buy(2, 105, 1)
        };
        let zero_price = Order {
            price: px(0),
            ..buy(3, 105, 1)
        };
        let long_negative = order(4, Side::Long, 105, -1);
        let short_positive = order(5, Side::Short, 105, 1);
        for bad in [zero_size, zero_price, long_negative, short_positive] {
            assert!(bad.validate().is_err());
            assert!(book.process_order(bad).is_empty());
        }
        assert_eq!(book.len(), 1, "only the original ask rests");
    }

    #[test]
    fn duplicate_live_order_id_is_rejected() {
        let mut book = OrderBook::new();
        book.process_order(buy(1, 100, 5));
        assert!(book.process_order(buy(1, 101, 5)).is_empty());
        assert_eq!(book.order(1).unwrap().price, px(100));

        // Once order 1 is gone, the id is free again.
        book.cancel_order(1).unwrap();
        assert!(book.process_order(buy(1, 101, 5)).is_empty());
        assert_eq!(book.order(1).unwrap().price, px(101));
    }

    #[test]
    fn matched_trade_serializes_for_on_chain_settlement() {
        let mut book = OrderBook::new();
        book.process_order(sell(1, 105, 3));
        let results = book.process_order(buy(2, 105, 3));
        let trade = &results[0].trade;

        // The settlement payload round-trips through JSON — the transport
        // the RPC adapter (todo #20) uses before ABI-encoding for
        // `SettlementBatcher.submitBatch`.
        let json = serde_json::to_string(trade).unwrap();
        let decoded: MatchedTrade = serde_json::from_str(&json).unwrap();
        assert_eq!(*trade, decoded);
        assert_eq!(decoded.size, 3);
        assert_eq!(decoded.price, px(105));
    }

    #[test]
    fn conservation_holds_across_a_scripted_sequence() {
        let mut book = OrderBook::new();
        let mut streamed_buy = 0u128;
        let mut streamed_sell = 0u128;
        let mut filled = 0u128;
        let mut removed_buy = 0u128;
        let mut removed_sell = 0u128;

        // Makers rest on both sides.
        book.process_order(sell(1, 103, 5));
        book.process_order(sell(2, 104, 5));
        book.process_order(buy(3, 99, 4));
        streamed_sell += 10;
        streamed_buy += 4;

        // A buy sweeps part of the ask side.
        for result in book.process_order(buy(4, 103, 3)) {
            filled += result.trade.size as u128;
        }
        streamed_buy += 3;

        // A sell sweeps the resting bid at 99 completely.
        for result in book.process_order(sell(5, 99, 4)) {
            filled += result.trade.size as u128;
        }
        streamed_sell += 4;

        // Cancel one resting ask (its 5 unfilled units leave the pool).
        removed_sell += book.cancel_order(2).unwrap().quantity();

        // Expire an order placed with a near expiry.
        let mut short_lived = buy(6, 98, 7);
        short_lived.expiry_block = 1;
        book.process_order(short_lived);
        streamed_buy += 7;
        removed_buy += 7;
        assert_eq!(book.cleanup_expired(1), vec![6]);

        // Conservation per side: streamed == filled + removed + resting.
        let resting_buy: u128 = [4u64, 3]
            .into_iter()
            .filter_map(|id| book.order(id))
            .map(|o| o.quantity())
            .sum();
        let resting_sell: u128 = book.order(1).map(|o| o.quantity()).unwrap_or(0);
        assert_eq!(streamed_buy, filled + removed_buy + resting_buy);
        assert_eq!(streamed_sell, filled + removed_sell + resting_sell);
        assert_eq!(filled, 3 + 4);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    /// Mid price the fuzz domain centers on (plain units — scaling is
    /// irrelevant to book mechanics).
    const MID: u128 = 1_000_000;
    /// Fuzz band: limit prices stay within ±5% of mid (plan todo #16).
    const BAND_LOW: u128 = MID * 95 / 100;
    const BAND_HIGH: u128 = MID * 105 / 100;
    const MAX_QTY: i128 = 1_000;
    const MAX_BLOCK: u64 = 64;

    fn side_index(side: Side) -> usize {
        match side {
            Side::Long => 0,
            Side::Short => 1,
        }
    }

    /// One stream item: (side, limit price, signed size, expiry block).
    fn stream_item() -> impl Strategy<Value = (Side, u128, i128, u64)> {
        (
            any::<bool>(),
            BAND_LOW..=BAND_HIGH,
            1i128..=MAX_QTY,
            0u64..=MAX_BLOCK,
        )
            .prop_map(|(is_buy, price, qty, expiry)| {
                if is_buy {
                    (Side::Long, price, qty, expiry)
                } else {
                    (Side::Short, price, -qty, expiry)
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// Fuzzes random limit-order streams within ±5% of mid and
        /// asserts the two plan-mandated invariants after every fill and
        /// at the end of the stream:
        ///
        /// - **Conservation of shares:** per side, every unit streamed
        ///   into the engine is accounted for as filled, recycled
        ///   (expired), or still resting — no shares are created or
        ///   destroyed, and every fill's two legs carry equal size.
        /// - **Bounded slippage:** no fill executes beyond the taker's
        ///   limit price (buys never pay more, sells never receive less),
        ///   every fill executes exactly at the resting maker's price,
        ///   and all execution prices stay inside the ±5% band.
        #[test]
        fn fuzzed_streams_preserve_conservation_and_bounded_slippage(
            items in prop::collection::vec(stream_item(), 1..=MAX_BLOCK as usize),
            cleanup_block in 0u64..=MAX_BLOCK,
        ) {
            let mut book = OrderBook::new();
            // Shadow state: id → (side, limit price, remaining quantity).
            let mut live: HashMap<OrderId, (Side, Price, u128)> = HashMap::new();
            let mut streamed = [0u128; 2];
            let mut filled = [0u128; 2];

            for (index, (side, limit, size, expiry)) in items.iter().enumerate() {
                let id = index as OrderId + 1;
                let order = Order {
                    id,
                    side: *side,
                    price: U256::from(*limit),
                    size: *size,
                    expiry_block: *expiry,
                    signature: vec![],
                };
                let qty = order.quantity();
                streamed[side_index(*side)] += qty;

                let results = book.process_order(order);
                for result in &results {
                    let fill = result.trade.size as u128;
                    prop_assert!(fill > 0, "fills are always positive");

                    // Bounded slippage against the taker's limit.
                    match side {
                        Side::Long => prop_assert!(result.trade.price <= U256::from(*limit)),
                        Side::Short => prop_assert!(result.trade.price >= U256::from(*limit)),
                    }
                    // Execution at the resting maker's price, and the
                    // price stays inside the fuzz band.
                    let (_, maker_price, _) = live
                        .get(&result.maker_order_id)
                        .expect("maker must be a live resting order");
                    prop_assert_eq!(result.trade.price, *maker_price);
                    prop_assert!(result.trade.price >= U256::from(BAND_LOW));
                    prop_assert!(result.trade.price <= U256::from(BAND_HIGH));

                    filled[0] += fill;
                    filled[1] += fill;

                    if result.maker_remaining == 0 {
                        live.remove(&result.maker_order_id);
                    } else {
                        live.get_mut(&result.maker_order_id).unwrap().2 =
                            result.maker_remaining.unsigned_abs();
                    }
                }

                // The taker's unfilled remainder rests (or the order was
                // fully filled / never crossed).
                let taker_resting = results
                    .last()
                    .map(|result| result.taker_remaining.unsigned_abs())
                    .unwrap_or(qty);
                if taker_resting > 0 {
                    live.insert(id, (*side, U256::from(*limit), taker_resting));
                }
            }
            prop_assert_eq!(filled[0], filled[1], "every fill has two equal legs");

            // Recycle expired orders and charge their remaining quantity
            // to the removal bucket of their side.
            let expired = book.cleanup_expired(cleanup_block);
            let mut removed = [0u128; 2];
            for id in &expired {
                let (side, _, remaining) =
                    live.remove(id).expect("expired id must be a live order");
                removed[side_index(side)] += remaining;
            }

            // Conservation of shares, per side:
            //   streamed == filled + recycled + resting.
            let resting_buy: u128 = live
                .values()
                .filter(|(side, _, _)| *side == Side::Long)
                .map(|(_, _, qty)| qty)
                .sum();
            let resting_sell: u128 = live
                .values()
                .filter(|(side, _, _)| *side == Side::Short)
                .map(|(_, _, qty)| qty)
                .sum();
            prop_assert_eq!(streamed[0], filled[0] + removed[0] + resting_buy);
            prop_assert_eq!(streamed[1], filled[1] + removed[1] + resting_sell);

            // The book and the shadow state agree on the survivors.
            prop_assert_eq!(book.len(), live.len());
        }
    }
}
