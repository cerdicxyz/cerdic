//! Price-time priority limit order book, built the way production
//! matching engines actually do it rather than the textbook
//! `BTreeMap<Price, VecDeque<Order>>` approach.
//!
//! # Why not a `BTreeMap` of price levels?
//!
//! A balanced tree gives O(log n) insert/remove per price level and,
//! worse, scatters price levels across the heap with no cache locality:
//! every level touched is a pointer chase to a new allocation. On the hot
//! path of a matching engine (insert, cancel, and walk-the-book-until-
//! filled, thousands of times a second), that adds up. It's *correct*,
//! it's just not what a shop running this at HFT latency would ship.
//!
//! # What this does instead
//!
//! Real engines (and this one) exploit the fact that price is already
//! discretized into ticks: instead of an ordered map keyed by an
//! arbitrary price, price levels live in a flat, densely-indexed array
//! (`Ladder`), so `price -> level` is a subtraction and an array index,
//! O(1) with no tree traversal and excellent cache locality (adjacent
//! price levels are adjacent in memory). Each level is a FIFO queue
//! implemented as an intrusive doubly-linked list over a slab arena of
//! order nodes (`Vec<Option<OrderNode>>`), not a `Box`/`Rc` per order.
//! Inserting, removing, and splicing a node is index arithmetic, not
//! allocator traffic. Order lookup for cancel is direct array indexing
//! (order IDs are arena slots, assigned by the book), not a `HashMap`, so
//! there's no hashing on the cancel path either.
//!
//! Net result: `add`, `cancel`, and single-order matching are all O(1)
//! amortized, and the constant factor is small (array indexing + a few
//! pointer-width writes) rather than tree-rebalancing or hash computation.
//!
//! # Finding the next occupied level: a hierarchical bitmap
//!
//! When the best-price level empties (fully filled or cancelled), the
//! book needs the next occupied level. A dense array makes that a scan,
//! fine on the happy path (price drifts gradually, the next level is
//! usually right there), bad on the adversarial one (a sweep clears a
//! wide contiguous range and the next resting order is far away).
//! `Occupancy` (see below) tracks level occupancy in a two-tier bitmap,
//! the same core trick Monad's on-chain "Octopus Heap" order book design
//! uses to skip empty price ticks, adapted here for worst-case-fast
//! in-memory lookup rather than to minimize gas-metered storage reads.
//! `next_set_from`/`prev_set_from` jump straight to the answer via a
//! handful of word-level bit operations instead of visiting every empty
//! level in between.
//!
//! # The one real tradeoff
//!
//! A dense array needs bounded, contiguous indices. `Ladder` grows to
//! cover whatever tick range has actually been quoted and never shrinks.
//! Fine for a market trading in a bounded band (which every real market
//! does, moment to moment), but a pathological order miles from the
//! touch would allocate a large, mostly-empty array. Production books
//! handle this with periodic re-basing (drop the array, rebuild centered
//! on the current touch) or a sparse fallback for far-out-of-range
//! resting orders; this implementation notes the tradeoff rather than
//! solving a problem this venue's realistic tick ranges don't create.

// Not wired into main.rs yet. The API layer (POST /order) that will call
// OrderBook::submit lands in a later milestone. Fully exercised by the
// tests below in the meantime.
#![allow(dead_code)]

use common::types::Side;

pub type OrderId = u32;
/// Price, in ticks. The caller owns the mapping from ticks to whatever
/// oracle-scaled unit (e.g. 1e18) the rest of the kernel uses.
pub type Tick = u64;
pub type Qty = u64;
/// Opaque owner identifier, used only for self-trade prevention. The
/// caller owns the mapping to a real identity (e.g. `portfolioKey`).
pub type OwnerId = u64;
/// Expiry timestamp, in whatever monotonic unit the caller's `now` (see
/// `OrderBook::submit`) is denominated in (block number, unix seconds,
/// etc.). The book itself is agnostic, it only ever compares `now` to a
/// stored expiry with `>=`.
pub type Timestamp = u64;

const NIL: u32 = u32::MAX;

fn opposite_side(side: Side) -> Side {
    match side {
        Side::Long => Side::Short,
        Side::Short => Side::Long,
    }
}

/// What to do with any quantity that doesn't fill immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimeInForce {
    /// Rest any unfilled remainder in the book until cancelled.
    GoodTilCancel,
    /// Rest the remainder, but drop it once `now >= expiry`, checked
    /// lazily, the moment matching would otherwise touch it, not on a
    /// timer (see module docs, "Lazy expiry").
    GoodTilTime(Timestamp),
    /// Fill whatever crosses immediately; cancel anything left over.
    /// Never rests.
    ImmediateOrCancel,
    /// All or nothing: fill the entire quantity immediately, or match
    /// none of it and rest nothing. No partial fills.
    FillOrKill,
}

/// A new order to submit. See `OrderBook::submit`.
#[derive(Debug, Clone, Copy)]
pub struct NewOrder {
    pub side: Side,
    pub tick: Tick,
    pub qty: Qty,
    pub owner: OwnerId,
    pub tif: TimeInForce,
    /// Reject the order outright if it would take any resting liquidity
    /// (cross the book), instead of only ever adding it. Standard
    /// maker-only semantics: an order that would cross is rejected in
    /// full, not truncated to its non-crossing portion.
    pub post_only: bool,
}

/// One resting order, as a node in a level's intrusive FIFO list.
#[derive(Debug, Clone, Copy)]
struct OrderNode {
    tick: Tick,
    qty: Qty,
    side: Side,
    owner: OwnerId,
    expiry: Option<Timestamp>,
    prev: u32,
    next: u32,
}

/// One price level: a FIFO queue of order-arena indices plus the level's
/// aggregate resting quantity (kept incrementally so depth queries don't
/// need to walk the list).
#[derive(Debug, Clone, Copy, Default)]
struct Level {
    head: u32,
    tail: u32,
    qty: Qty,
}

impl Level {
    const EMPTY: Level = Level { head: NIL, tail: NIL, qty: 0 };

    fn is_empty(&self) -> bool {
        self.head == NIL
    }
}

/// Hierarchical occupancy bitmap over a `Ladder`'s levels: bit `i` set
/// means level `i` has resting orders. Two tiers, `words` (one bit per
/// level) and `summary` (one bit per `words` entry, set iff that word is
/// nonzero), so finding the next/previous occupied level never has to
/// visit an empty one individually.
///
/// This is the same core trick Monad's "Octopus Heap" on-chain order
/// book design uses to jump between active price regions without
/// scanning empty ticks. There, it's to avoid gas-metered storage reads;
/// here, it's to give worst-case fast level lookup instead of relying on the
/// happy-path assumption that price only ever drifts a short distance
/// between touches. A resting order at one edge of a wide, mostly-empty
/// book and a sweep that clears everything in between is exactly the
/// case a plain linear scan handles badly and this doesn't.
#[derive(Default)]
struct Occupancy {
    words: Vec<u64>,
    summary: Vec<u64>,
}

impl Occupancy {
    fn ensure_len(&mut self, level_count: usize) {
        let word_count = level_count.div_ceil(64);
        if word_count > self.words.len() {
            self.words.resize(word_count, 0);
            self.summary.resize(word_count.div_ceil(64), 0);
        }
    }

    /// Shifts every set bit up by `shift` positions, the bitmap's side
    /// of `Ladder::ensure`'s prepend-on-rebase. O(current size), the same
    /// cost class as the array prepend it accompanies.
    fn shift_up(&mut self, shift: usize, new_level_count: usize) {
        let mut shifted = Occupancy::default();
        shifted.ensure_len(new_level_count);
        for i in 0..self.words.len() * 64 {
            if self.is_set(i) {
                shifted.set(i + shift);
            }
        }
        *self = shifted;
    }

    fn set(&mut self, idx: usize) {
        self.ensure_len(idx + 1);
        let word = idx / 64;
        self.words[word] |= 1u64 << (idx % 64);
        self.summary[word / 64] |= 1u64 << (word % 64);
    }

    fn clear(&mut self, idx: usize) {
        let word = idx / 64;
        if word >= self.words.len() {
            return;
        }
        self.words[word] &= !(1u64 << (idx % 64));
        if self.words[word] == 0 {
            self.summary[word / 64] &= !(1u64 << (word % 64));
        }
    }

    fn is_set(&self, idx: usize) -> bool {
        let word = idx / 64;
        word < self.words.len() && self.words[word] & (1u64 << (idx % 64)) != 0
    }

    /// First set bit at or after `idx`, if any.
    fn next_set_from(&self, idx: usize) -> Option<usize> {
        let start_word = idx / 64;
        if start_word >= self.words.len() {
            return None;
        }
        let masked = self.words[start_word] & (!0u64 << (idx % 64));
        if masked != 0 {
            return Some(start_word * 64 + masked.trailing_zeros() as usize);
        }
        let mut sw = start_word / 64;
        let mut mask_from_bit = (start_word % 64) + 1;
        while sw < self.summary.len() {
            let effective = if mask_from_bit >= 64 { 0 } else { self.summary[sw] & (!0u64 << mask_from_bit) };
            if effective != 0 {
                let word = sw * 64 + effective.trailing_zeros() as usize;
                if word >= self.words.len() {
                    return None;
                }
                let w = self.words[word];
                debug_assert_ne!(w, 0);
                return Some(word * 64 + w.trailing_zeros() as usize);
            }
            sw += 1;
            mask_from_bit = 0;
        }
        None
    }

    /// Last set bit at or before `idx`, if any.
    fn prev_set_from(&self, idx: usize) -> Option<usize> {
        if self.words.is_empty() {
            return None;
        }
        let start_word = idx / 64;
        if start_word >= self.words.len() {
            return self.prev_set_from(self.words.len() * 64 - 1);
        }
        let bit = idx % 64;
        let masked = self.words[start_word] & (!0u64 >> (63 - bit));
        if masked != 0 {
            return Some(start_word * 64 + (63 - masked.leading_zeros() as usize));
        }
        if start_word == 0 {
            return None;
        }
        let mut sw = start_word - 1;
        loop {
            let summary_bit = sw % 64;
            let summary_word = sw / 64;
            let effective = self.summary[summary_word] & (!0u64 >> (63 - summary_bit));
            if effective != 0 {
                let word = summary_word * 64 + (63 - effective.leading_zeros() as usize);
                let w = self.words[word];
                debug_assert_ne!(w, 0);
                return Some(word * 64 + (63 - w.leading_zeros() as usize));
            }
            if summary_word == 0 {
                return None;
            }
            sw = summary_word * 64 - 1;
        }
    }
}

/// A dense, tick-indexed array of price levels for one side of the book.
/// Grows on demand to cover `[min_tick, max_tick]` as orders arrive
/// outside its current range; never shrinks (see module docs).
struct Ladder {
    base_tick: Tick,
    levels: Vec<Level>,
    occupancy: Occupancy,
}

impl Ladder {
    fn new() -> Self {
        Self { base_tick: 0, levels: Vec::new(), occupancy: Occupancy::default() }
    }

    /// Ensures `tick` has a slot, growing/re-basing the array if needed,
    /// and returns its index.
    fn ensure(&mut self, tick: Tick) -> usize {
        if self.levels.is_empty() {
            self.base_tick = tick;
            self.levels.push(Level::EMPTY);
            self.occupancy.ensure_len(1);
            return 0;
        }
        if tick < self.base_tick {
            // Re-base: prepend enough empty levels to cover the new low.
            let shift = (self.base_tick - tick) as usize;
            let mut grown = vec![Level::EMPTY; shift];
            grown.extend_from_slice(&self.levels);
            self.levels = grown;
            self.occupancy.shift_up(shift, self.levels.len());
            self.base_tick = tick;
            return 0;
        }
        let idx = (tick - self.base_tick) as usize;
        if idx >= self.levels.len() {
            self.levels.resize(idx + 1, Level::EMPTY);
            self.occupancy.ensure_len(idx + 1);
        }
        idx
    }

    /// Marks `idx` as occupied (called whenever a level goes from empty
    /// to non-empty).
    fn mark_occupied(&mut self, idx: usize) {
        self.occupancy.set(idx);
    }

    /// Marks `idx` as empty (called whenever a level's last order is
    /// removed).
    fn mark_empty(&mut self, idx: usize) {
        self.occupancy.clear(idx);
    }

    /// Next occupied level at or after `idx`, via the occupancy bitmap.
    /// O(1) amortized, not a per-level scan.
    fn next_occupied_from(&self, idx: usize) -> Option<usize> {
        self.occupancy.next_set_from(idx)
    }

    /// Last occupied level at or before `idx`, via the occupancy bitmap.
    fn prev_occupied_from(&self, idx: usize) -> Option<usize> {
        self.occupancy.prev_set_from(idx)
    }

    /// Next occupied level strictly worse than `from_idx` on this side.
    /// Bids get worse as the index decreases (best bid is the highest
    /// tick), asks get worse as the index increases (best ask is the
    /// lowest tick). Shared by `advance_best_from` (mutating) and
    /// `available_to_match` (read-only), so the two can't drift apart on
    /// which direction "worse" means.
    fn next_worse_level(&self, side: Side, from_idx: usize) -> Option<usize> {
        match side {
            Side::Long => from_idx.checked_sub(1).and_then(|i| self.prev_occupied_from(i)),
            Side::Short => self.next_occupied_from(from_idx + 1),
        }
    }

    /// Index for `tick` if that level has ever been allocated (does not
    /// grow the array, used on the read/cancel path where a miss just
    /// means "no such level").
    fn index_of(&self, tick: Tick) -> Option<usize> {
        if self.levels.is_empty() || tick < self.base_tick {
            return None;
        }
        let idx = (tick - self.base_tick) as usize;
        (idx < self.levels.len()).then_some(idx)
    }

    fn level(&self, idx: usize) -> &Level {
        &self.levels[idx]
    }

    fn level_mut(&mut self, idx: usize) -> &mut Level {
        &mut self.levels[idx]
    }

    fn tick_at(&self, idx: usize) -> Tick {
        self.base_tick + idx as u64
    }
}

/// A single fill produced by matching an incoming order against resting
/// liquidity. `maker_id` is the resting order that got hit; `taker`
/// quantity crossed is `qty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    pub maker_id: OrderId,
    pub maker_owner: OwnerId,
    pub tick: Tick,
    pub qty: Qty,
    /// True when the maker order was fully consumed and removed from the
    /// book (as opposed to partially filled and left resting).
    pub maker_filled: bool,
    /// The maker order's resting size immediately before this fill
    /// (i.e. `qty <= maker_size_before` always holds). Not derivable
    /// from `maker_filled` alone, and needed as-is (not just the
    /// post-fill remainder) by `MatchCorrectness`'s `matchSize <=
    /// size_a` constraint, which is a statement about the size that
    /// was live at match time, not what's left afterward.
    pub maker_size_before: Qty,
    /// The taker's own remaining size at the moment this specific fill
    /// happened (relevant when one incoming order sweeps multiple
    /// resting orders across one or more price levels: each `Fill` in
    /// that sweep has a different `taker_size_before`, the taker's
    /// remaining quantity right before *that* fill, not the order's
    /// original total).
    pub taker_size_before: Qty,
}

/// Result of submitting a new order: whatever it matched immediately,
/// plus the id it was assigned if any quantity is left resting.
#[derive(Debug, Clone, Default)]
pub struct SubmitResult {
    pub fills: Vec<Fill>,
    pub resting_id: Option<OrderId>,
    pub resting_qty: Qty,
    /// Resting orders the matching walk removed because they'd expired
    /// (`now >= expiry`) when it reached them. Not traded against, just
    /// pruned in passing. The caller should treat these as cancelled,
    /// not filled.
    pub expired: Vec<OrderId>,
    /// Resting orders cancelled because they belonged to the same
    /// `owner` as this incoming order (self-trade prevention, "cancel
    /// resting" policy: the standing order loses, not the incoming
    /// one). Not traded against.
    pub self_trade_cancelled: Vec<OrderId>,
    /// Set when `post_only` was true and the order would have crossed
    /// the book. The order was rejected in full: `fills` is empty and
    /// nothing rests.
    pub post_only_rejected: bool,
    /// Set when `tif` was `FillOrKill` and the book could not fill the
    /// entire quantity immediately. Nothing was matched or rested.
    pub fill_or_kill_failed: bool,
}

/// Price-time priority limit order book for one market. See module docs
/// for the architecture.
pub struct OrderBook {
    bids: Ladder,
    asks: Ladder,
    arena: Vec<Option<OrderNode>>,
    free: Vec<u32>,
    best_bid: Option<Tick>,
    best_ask: Option<Tick>,
}

impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: Ladder::new(),
            asks: Ladder::new(),
            arena: Vec::new(),
            free: Vec::new(),
            best_bid: None,
            best_ask: None,
        }
    }

    pub fn best_bid(&self) -> Option<Tick> {
        self.best_bid
    }

    pub fn best_ask(&self) -> Option<Tick> {
        self.best_ask
    }

    /// Resting quantity at `tick` on `side`, 0 if the level doesn't exist
    /// or is empty.
    pub fn depth_at(&self, side: Side, tick: Tick) -> Qty {
        let ladder = self.ladder(side);
        ladder.index_of(tick).map(|i| ladder.level(i).qty).unwrap_or(0)
    }

    fn ladder(&self, side: Side) -> &Ladder {
        match side {
            Side::Long => &self.bids,
            Side::Short => &self.asks,
        }
    }

    fn ladder_mut(&mut self, side: Side) -> &mut Ladder {
        match side {
            Side::Long => &mut self.bids,
            Side::Short => &mut self.asks,
        }
    }

    fn alloc(&mut self, node: OrderNode) -> u32 {
        if let Some(idx) = self.free.pop() {
            self.arena[idx as usize] = Some(node);
            idx
        } else {
            self.arena.push(Some(node));
            (self.arena.len() - 1) as u32
        }
    }

    /// Accesses a live arena slot. Every call site holds an index that
    /// came from a level's `head`/`tail`, or a node's `prev`/`next`,
    /// which by construction is either `NIL` (checked separately by the
    /// caller) or a currently-live slot. A slot ever going missing here
    /// means the arena's own invariants broke, not a normal runtime
    /// condition, hence the panic rather than an `Option` return the
    /// caller would have to handle everywhere for a case that can't
    /// legitimately happen.
    fn slot(&self, idx: u32) -> &OrderNode {
        self.arena[idx as usize]
            .as_ref()
            .unwrap_or_else(|| panic!("book invariant violated: arena slot {idx} referenced but empty"))
    }

    fn slot_mut(&mut self, idx: u32) -> &mut OrderNode {
        self.arena[idx as usize]
            .as_mut()
            .unwrap_or_else(|| panic!("book invariant violated: arena slot {idx} referenced but empty"))
    }

    /// Removes and returns a live arena slot's contents, freeing it for
    /// reuse. Same invariant as `slot`/`slot_mut`.
    fn take_slot(&mut self, idx: u32) -> OrderNode {
        self.arena[idx as usize]
            .take()
            .unwrap_or_else(|| panic!("book invariant violated: arena slot {idx} referenced but empty"))
    }

    fn push_back(&mut self, side: Side, tick: Tick, idx: u32) {
        let level_idx = self.ladder_mut(side).ensure(tick);
        let ladder = self.ladder_mut(side);
        let tail = ladder.level(level_idx).tail;
        if tail == NIL {
            let level = ladder.level_mut(level_idx);
            level.head = idx;
            level.tail = idx;
            self.ladder_mut(side).mark_occupied(level_idx);
        } else {
            self.slot_mut(tail).next = idx;
            self.slot_mut(idx).prev = tail;
            self.ladder_mut(side).level_mut(level_idx).tail = idx;
        }
        let qty = self.slot(idx).qty;
        self.ladder_mut(side).level_mut(level_idx).qty += qty;
        self.raise_best(side, tick);
    }

    /// Updates the cached best price after a new resting order lands.
    fn raise_best(&mut self, side: Side, tick: Tick) {
        match side {
            Side::Long => {
                if self.best_bid.map_or(true, |b| tick > b) {
                    self.best_bid = Some(tick);
                }
            }
            Side::Short => {
                if self.best_ask.map_or(true, |a| tick < a) {
                    self.best_ask = Some(tick);
                }
            }
        }
    }

    /// After the level at `tick` on `side` has just emptied, advances the
    /// cached best price to the next occupied level via the occupancy
    /// bitmap, jumping directly to it instead of scanning intervening
    /// empty levels one at a time.
    fn advance_best_from(&mut self, side: Side, emptied_tick: Tick) {
        let ladder = self.ladder(side);
        let Some(start) = ladder.index_of(emptied_tick) else { return };
        let next = self.ladder(side).next_worse_level(side, start).map(|i| self.ladder(side).tick_at(i));
        match side {
            Side::Long => self.best_bid = next,
            Side::Short => self.best_ask = next,
        }
    }

    /// Removes a live order's arena node from its level's linked list and
    /// frees the slot. Does not touch `best_bid`/`best_ask`; the caller
    /// decides whether the level emptied and advances the cursor.
    fn unlink(&mut self, idx: u32) -> OrderNode {
        let node = self.take_slot(idx);
        self.free.push(idx);

        let ladder = self.ladder_mut(node.side);
        let Some(level_idx) = ladder.index_of(node.tick) else { return node };

        if node.prev != NIL {
            self.slot_mut(node.prev).next = node.next;
        } else {
            self.ladder_mut(node.side).level_mut(level_idx).head = node.next;
        }
        if node.next != NIL {
            self.slot_mut(node.next).prev = node.prev;
        } else {
            self.ladder_mut(node.side).level_mut(level_idx).tail = node.prev;
        }
        let level = self.ladder_mut(node.side).level_mut(level_idx);
        level.qty -= node.qty;
        if level.is_empty() {
            self.ladder_mut(node.side).mark_empty(level_idx);
        }

        node
    }

    /// Cancels a live resting order in O(1). Returns its remaining
    /// quantity and price if it was still live.
    pub fn cancel(&mut self, id: OrderId) -> Option<(Tick, Qty)> {
        let idx = id;
        if (idx as usize) >= self.arena.len() || self.arena[idx as usize].is_none() {
            return None;
        }
        let side = self.slot(idx).side;
        let tick = self.slot(idx).tick;
        let node = self.unlink(idx);

        let now_empty =
            self.ladder(side).index_of(tick).map(|i| self.ladder(side).level(i).is_empty()).unwrap_or(true);
        let was_best = match side {
            Side::Long => self.best_bid == Some(tick),
            Side::Short => self.best_ask == Some(tick),
        };
        if now_empty && was_best {
            self.advance_best_from(side, tick);
        }
        Some((tick, node.qty))
    }

    /// Read-only walk mirroring `submit`'s matching loop, used only to
    /// answer "could a Fill-or-Kill order for `qty` be fully satisfied
    /// right now?" before committing any state change. Must skip exactly
    /// the same orders `submit` would skip (expired, self-owned). If
    /// this ever drifts from `submit`'s real skip logic, FOK can
    /// overestimate available liquidity and let a partial fill through
    /// as if it were all-or-nothing, which is the one bug this order
    /// type exists specifically to prevent.
    fn available_to_match(&self, side: Side, tick: Tick, needed: Qty, owner: OwnerId, now: Timestamp) -> Qty {
        let opposite = opposite_side(side);
        let mut level_tick = match opposite {
            Side::Long => self.best_bid,
            Side::Short => self.best_ask,
        };
        let mut total: Qty = 0;

        while let Some(lt) = level_tick {
            let crosses = match side {
                Side::Long => tick >= lt,
                Side::Short => tick <= lt,
            };
            if !crosses {
                break;
            }
            let Some(level_idx) = self.ladder(opposite).index_of(lt) else { break };

            let mut cur = self.ladder(opposite).level(level_idx).head;
            while cur != NIL {
                let node = self.slot(cur);
                let expired = node.expiry.is_some_and(|e| now >= e);
                if !expired && node.owner != owner {
                    total = total.saturating_add(node.qty);
                    if total >= needed {
                        return total;
                    }
                }
                cur = node.next;
            }

            level_tick = self
                .ladder(opposite)
                .next_worse_level(opposite, level_idx)
                .map(|i| self.ladder(opposite).tick_at(i));
        }

        total
    }

    /// Submits a new order per `order.tif`/`order.post_only`: matches
    /// immediately against the opposite side at prices that cross, then
    /// rests any remainder (unless `tif` says not to). Price-time
    /// priority: at each price level, the resting order that arrived
    /// first (the level's FIFO head) fills first — except any head found
    /// expired or owned by `order.owner` is pruned/cancelled in passing
    /// rather than matched.
    ///
    /// `now` is the caller's clock (block number, unix seconds — the
    /// book doesn't care which, as long as it's consistent with whatever
    /// `TimeInForce::GoodTilTime` expiries were stamped with). Passed
    /// in, not read from a system clock, so matching stays deterministic
    /// and testable.
    pub fn submit(&mut self, order: NewOrder, now: Timestamp) -> SubmitResult {
        let mut result = SubmitResult::default();
        let NewOrder { side, tick, mut qty, owner, tif, post_only: _ } = order;
        let opposite = opposite_side(side);

        if order.post_only {
            let would_cross = match opposite {
                Side::Long => self.best_bid.is_some_and(|bid| tick <= bid),
                Side::Short => self.best_ask.is_some_and(|ask| tick >= ask),
            };
            if would_cross {
                result.post_only_rejected = true;
                return result;
            }
        }

        if tif == TimeInForce::FillOrKill && self.available_to_match(side, tick, qty, owner, now) < qty {
            result.fill_or_kill_failed = true;
            return result;
        }

        while qty > 0 {
            let opposite_best = match opposite {
                Side::Long => self.best_bid,
                Side::Short => self.best_ask,
            };
            let Some(level_tick) = opposite_best else { break };
            let crosses = match side {
                Side::Long => tick >= level_tick,
                Side::Short => tick <= level_tick,
            };
            if !crosses {
                break;
            }

            let level_idx = self
                .ladder(opposite)
                .index_of(level_tick)
                .expect("best_bid/best_ask must always reference an allocated level (raise_best only ever sets them to a tick that ensure() just allocated)");
            let head = self.ladder(opposite).level(level_idx).head;
            debug_assert_ne!(head, NIL, "cached best price must point at a non-empty level");

            let head_node = self.slot(head);
            let expired = head_node.expiry.is_some_and(|e| now >= e);
            let self_trade = head_node.owner == owner;

            if expired || self_trade {
                self.unlink(head);
                if expired {
                    result.expired.push(head);
                } else {
                    result.self_trade_cancelled.push(head);
                }
                if self.ladder(opposite).level(level_idx).is_empty() {
                    self.advance_best_from(opposite, level_tick);
                }
                continue;
            }

            let maker_qty = head_node.qty;
            let maker_owner = head_node.owner;
            let taker_size_before = qty;
            let traded = qty.min(maker_qty);
            qty -= traded;

            let maker_filled = traded == maker_qty;
            if maker_filled {
                // unlink() subtracts the node's full (untouched) qty from
                // the level's aggregate itself — do not also subtract
                // `traded` here, that would double-count it.
                self.unlink(head);
            } else {
                self.slot_mut(head).qty -= traded;
                self.ladder_mut(opposite).level_mut(level_idx).qty -= traded;
            }

            result.fills.push(Fill {
                maker_id: head,
                maker_owner,
                tick: level_tick,
                qty: traded,
                maker_filled,
                maker_size_before: maker_qty,
                taker_size_before,
            });

            if self.ladder(opposite).level(level_idx).is_empty() {
                self.advance_best_from(opposite, level_tick);
            }
        }

        if qty > 0 && !matches!(tif, TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill) {
            let expiry = if let TimeInForce::GoodTilTime(t) = tif { Some(t) } else { None };
            let node = OrderNode { tick, qty, side, owner, expiry, prev: NIL, next: NIL };
            let idx = self.alloc(node);
            self.push_back(side, tick, idx);
            result.resting_id = Some(idx);
            result.resting_qty = qty;
        }

        result
    }
}

#[cfg(test)]
mod occupancy_tests {
    use super::Occupancy;
    use proptest::prelude::*;

    fn naive_next(set: &[usize], from: usize) -> Option<usize> {
        set.iter().copied().filter(|&i| i >= from).min()
    }

    fn naive_prev(set: &[usize], from: usize) -> Option<usize> {
        set.iter().copied().filter(|&i| i <= from).max()
    }

    #[test]
    fn single_bit_found_forward_and_backward() {
        let mut o = Occupancy::default();
        o.set(200);
        assert_eq!(o.next_set_from(0), Some(200));
        assert_eq!(o.next_set_from(200), Some(200));
        assert_eq!(o.next_set_from(201), None);
        assert_eq!(o.prev_set_from(300), Some(200));
        assert_eq!(o.prev_set_from(200), Some(200));
        assert_eq!(o.prev_set_from(199), None);
    }

    #[test]
    fn crosses_word_boundary() {
        // Bits 63 and 64 straddle the first two u64 words.
        let mut o = Occupancy::default();
        o.set(64);
        assert_eq!(o.next_set_from(63), Some(64));
        assert_eq!(o.prev_set_from(64), Some(64));
        assert_eq!(o.prev_set_from(63), None);
    }

    #[test]
    fn crosses_summary_boundary() {
        // Word 64 is bit 0 of the *second* summary word (64 words * 64
        // bits/word = bit 4096) — this only works if the two-tier jump
        // is wired correctly across summary words, not just within one.
        let mut o = Occupancy::default();
        o.set(4096);
        assert_eq!(o.next_set_from(0), Some(4096));
        assert_eq!(o.next_set_from(4096), Some(4096));
        assert_eq!(o.next_set_from(4097), None);
        assert_eq!(o.prev_set_from(10_000), Some(4096));
    }

    #[test]
    fn empty_bitmap_finds_nothing() {
        let o = Occupancy::default();
        assert_eq!(o.next_set_from(0), None);
        assert_eq!(o.prev_set_from(0), None);
    }

    #[test]
    fn clear_removes_bit_and_summary_when_word_empties() {
        let mut o = Occupancy::default();
        o.set(10);
        o.clear(10);
        assert_eq!(o.next_set_from(0), None);
        assert_eq!(o.prev_set_from(100), None);
    }

    proptest! {
        /// Differential test: for arbitrary sets of bits and arbitrary
        /// query points, the bitmap's forward/backward jump must agree
        /// with a naive brute-force scan over the same set. This is the
        /// real correctness guarantee for a two-tier bit-trick structure
        /// like this one — word/summary boundary bugs don't show up in
        /// hand-picked examples reliably.
        #[test]
        fn matches_naive_scan(
            bits in prop::collection::hash_set(0usize..2000, 0..40),
            queries in prop::collection::vec(0usize..2000, 1..20),
        ) {
            let mut o = Occupancy::default();
            let set: Vec<usize> = bits.into_iter().collect();
            for &b in &set {
                o.set(b);
            }
            for &q in &queries {
                prop_assert_eq!(o.next_set_from(q), naive_next(&set, q));
                prop_assert_eq!(o.prev_set_from(q), naive_prev(&set, q));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Good-til-cancel order from `owner`, not post-only. Tests that
    /// don't care about self-trade prevention use distinct owners for
    /// maker/taker (as real distinct traders would be) so the new
    /// same-owner-cancels-resting behavior doesn't change their meaning.
    fn gtc(side: Side, tick: Tick, qty: Qty, owner: OwnerId) -> NewOrder {
        NewOrder { side, tick, qty, owner, tif: TimeInForce::GoodTilCancel, post_only: false }
    }

    const MAKER: OwnerId = 1;
    const TAKER: OwnerId = 2;

    #[test]
    fn resting_order_sets_best_price() {
        let mut book = OrderBook::new();
        let r = book.submit(gtc(Side::Long, 100, 10, MAKER), 0);
        assert_eq!(r.resting_qty, 10);
        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn crossing_order_fills_immediately() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 5, MAKER), 0);
        let r = book.submit(gtc(Side::Long, 100, 5, TAKER), 0);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].qty, 5);
        assert!(r.fills[0].maker_filled);
        assert_eq!(r.resting_qty, 0);
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn partial_fill_leaves_maker_resting() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 10, MAKER), 0);
        let r = book.submit(gtc(Side::Long, 100, 4, TAKER), 0);
        assert_eq!(r.fills[0].qty, 4);
        assert!(!r.fills[0].maker_filled);
        assert_eq!(book.depth_at(Side::Short, 100), 6);
        assert_eq!(book.best_ask(), Some(100));
    }

    #[test]
    fn fifo_within_a_level() {
        let mut book = OrderBook::new();
        let first = book.submit(gtc(Side::Short, 100, 5, MAKER), 0).resting_id.unwrap();
        let _second = book.submit(gtc(Side::Short, 100, 5, MAKER), 0).resting_id.unwrap();
        let r = book.submit(gtc(Side::Long, 100, 5, TAKER), 0);
        assert_eq!(r.fills[0].maker_id, first, "earlier resting order must fill first");
    }

    #[test]
    fn walks_multiple_price_levels_by_price_priority() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 101, 5, MAKER), 0); // worse price
        book.submit(gtc(Side::Short, 100, 5, MAKER), 0); // best price
        let r = book.submit(gtc(Side::Long, 101, 10, TAKER), 0);
        assert_eq!(r.fills.len(), 2);
        assert_eq!(r.fills[0].tick, 100, "best (lowest ask) price must fill first");
        assert_eq!(r.fills[1].tick, 101);
        assert_eq!(r.resting_qty, 0);
    }

    #[test]
    fn fill_reports_each_side_size_at_the_moment_of_that_fill() {
        // A 10-unit taker sweeps two 5-unit makers at different levels.
        // Each Fill must report the sizes live at THAT match, not the
        // taker's original total or the maker's post-fill remainder.
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 5, MAKER), 0);
        book.submit(gtc(Side::Short, 101, 5, MAKER), 0);
        let r = book.submit(gtc(Side::Long, 101, 10, TAKER), 0);

        assert_eq!(r.fills.len(), 2);
        assert_eq!(r.fills[0].maker_size_before, 5, "first maker's own resting size");
        assert_eq!(r.fills[0].taker_size_before, 10, "taker's full size going into the first fill");
        assert_eq!(r.fills[0].qty, 5);
        assert_eq!(r.fills[1].maker_size_before, 5, "second maker's own resting size");
        assert_eq!(
            r.fills[1].taker_size_before, 5,
            "taker only had 5 left after the first fill consumed 5 of its 10"
        );
        assert_eq!(r.fills[1].qty, 5);
    }

    #[test]
    fn cancel_removes_order_and_advances_best() {
        let mut book = OrderBook::new();
        let low = book.submit(gtc(Side::Long, 99, 5, MAKER), 0).resting_id.unwrap();
        let high = book.submit(gtc(Side::Long, 100, 5, MAKER), 0).resting_id.unwrap();
        assert_eq!(book.best_bid(), Some(100));

        let cancelled = book.cancel(high);
        assert_eq!(cancelled, Some((100, 5)));
        assert_eq!(book.best_bid(), Some(99), "best must fall back to next occupied level");

        assert_eq!(book.cancel(low), Some((99, 5)));
        assert_eq!(book.best_bid(), None);
    }

    #[test]
    fn cancel_unknown_id_is_none() {
        let mut book = OrderBook::new();
        assert_eq!(book.cancel(999), None);
    }

    #[test]
    fn resting_order_below_current_base_rebases_ladder() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Long, 100, 1, MAKER), 0);
        book.submit(gtc(Side::Long, 50, 1, MAKER), 0); // forces the ladder to re-base downward
        assert_eq!(book.depth_at(Side::Long, 100), 1);
        assert_eq!(book.depth_at(Side::Long, 50), 1);
        assert_eq!(book.best_bid(), Some(100));
    }

    #[test]
    fn arena_slot_is_reused_after_cancel() {
        let mut book = OrderBook::new();
        let a = book.submit(gtc(Side::Long, 100, 1, MAKER), 0).resting_id.unwrap();
        book.cancel(a).unwrap();
        let b = book.submit(gtc(Side::Long, 100, 1, MAKER), 0).resting_id.unwrap();
        assert_eq!(a, b, "freed arena slots must be recycled, not grown unboundedly");
    }

    // --- New order semantics: self-trade prevention, IOC, FOK, GTT expiry, post-only ---

    #[test]
    fn self_trade_cancels_resting_order_instead_of_filling() {
        let mut book = OrderBook::new();
        let resting = book.submit(gtc(Side::Short, 100, 5, MAKER), 0).resting_id.unwrap();
        let r = book.submit(gtc(Side::Long, 100, 5, MAKER), 0); // same owner as resting
        assert!(r.fills.is_empty(), "must not trade against its own resting order");
        assert_eq!(r.self_trade_cancelled, vec![resting]);
        assert_eq!(book.best_ask(), None, "the self-owned resting order must be gone, not just skipped");
        // The cancelled order's arena slot is immediately recycled (see
        // arena_slot_is_reused_after_cancel), so the taker's own resting
        // remainder reuses the same id, not a fresh one.
        assert_eq!(r.resting_id, Some(resting), "the taker's own remaining qty still rests");
    }

    #[test]
    fn self_trade_prevention_falls_through_to_a_different_owner() {
        let mut book = OrderBook::new();
        let mine = book.submit(gtc(Side::Short, 100, 5, MAKER), 0).resting_id.unwrap();
        let theirs = book.submit(gtc(Side::Short, 100, 5, TAKER), 0).resting_id.unwrap();
        let r = book.submit(gtc(Side::Long, 100, 5, MAKER), 0);
        assert_eq!(r.self_trade_cancelled, vec![mine]);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].maker_id, theirs);
        assert_eq!(r.fills[0].maker_owner, TAKER);
    }

    #[test]
    fn ioc_drops_unfilled_remainder_instead_of_resting() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 3, MAKER), 0);
        let order = NewOrder { tif: TimeInForce::ImmediateOrCancel, ..gtc(Side::Long, 100, 10, TAKER) };
        let r = book.submit(order, 0);
        assert_eq!(r.fills[0].qty, 3);
        assert_eq!(r.resting_id, None, "IOC must never rest a remainder");
        assert_eq!(r.resting_qty, 0);
        assert_eq!(book.best_bid(), None);
    }

    #[test]
    fn fok_fails_whole_order_when_liquidity_insufficient() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 3, MAKER), 0);
        let order = NewOrder { tif: TimeInForce::FillOrKill, ..gtc(Side::Long, 100, 10, TAKER) };
        let r = book.submit(order, 0);
        assert!(r.fill_or_kill_failed);
        assert!(r.fills.is_empty(), "a failed FOK must not partially fill");
        assert_eq!(book.depth_at(Side::Short, 100), 3, "the untouched maker order must still be resting");
    }

    #[test]
    fn fok_fills_fully_when_liquidity_sufficient_across_levels() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 4, MAKER), 0);
        book.submit(gtc(Side::Short, 101, 4, MAKER), 0);
        let order = NewOrder { tif: TimeInForce::FillOrKill, ..gtc(Side::Long, 101, 8, TAKER) };
        let r = book.submit(order, 0);
        assert!(!r.fill_or_kill_failed);
        assert_eq!(r.fills.iter().map(|f| f.qty).sum::<Qty>(), 8);
        assert_eq!(r.resting_id, None);
    }

    #[test]
    fn expired_resting_order_is_pruned_not_matched() {
        let mut book = OrderBook::new();
        let order = NewOrder { tif: TimeInForce::GoodTilTime(100), ..gtc(Side::Short, 100, 5, MAKER) };
        let expired = book.submit(order, 0).resting_id.unwrap();
        let r = book.submit(gtc(Side::Long, 100, 5, TAKER), 100); // now == expiry, so expired
        assert!(r.fills.is_empty());
        assert_eq!(r.expired, vec![expired]);
        assert_eq!(book.best_ask(), None);
        // Same arena-slot recycling as arena_slot_is_reused_after_cancel.
        assert_eq!(r.resting_id, Some(expired), "taker's qty rests since nothing was left to match");
    }

    #[test]
    fn unexpired_gtt_order_still_matches_normally() {
        let mut book = OrderBook::new();
        let order = NewOrder { tif: TimeInForce::GoodTilTime(100), ..gtc(Side::Short, 100, 5, MAKER) };
        book.submit(order, 0);
        let r = book.submit(gtc(Side::Long, 100, 5, TAKER), 50); // now < expiry
        assert_eq!(r.fills.len(), 1);
        assert!(r.expired.is_empty());
    }

    #[test]
    fn post_only_rejects_a_crossing_order_outright() {
        let mut book = OrderBook::new();
        book.submit(gtc(Side::Short, 100, 5, MAKER), 0);
        let order = NewOrder { post_only: true, ..gtc(Side::Long, 100, 5, TAKER) };
        let r = book.submit(order, 0);
        assert!(r.post_only_rejected);
        assert!(r.fills.is_empty());
        assert_eq!(r.resting_id, None, "a rejected post-only order rests nothing, not even its own quantity");
        assert_eq!(book.depth_at(Side::Short, 100), 5, "the resting maker order must be untouched");
    }

    #[test]
    fn post_only_rests_normally_when_it_would_not_cross() {
        let mut book = OrderBook::new();
        let order = NewOrder { post_only: true, ..gtc(Side::Long, 100, 5, MAKER) };
        let r = book.submit(order, 0);
        assert!(!r.post_only_rejected);
        assert_eq!(r.resting_qty, 5);
    }
}
