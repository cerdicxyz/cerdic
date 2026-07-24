//! Throughput benchmarks for the CLOB matching engine (plan todo #16).
//!
//! Acceptance: matching throughput ≥ 5000 matches/s on the test machine.
//! Each benchmark reports criterion's element throughput where one
//! element == one produced match (one [`MatchResult`]):
//!
//! - `single_match` — the full `process_order` path for one fill against
//!   a one-order book (map descent + fill + level eviction).
//! - `taker_sweeps_N_levels` — a taker sized to consume N single-order
//!   ask levels, measuring the per-match cost of the sweep loop,
//!   including per-level `BTreeMap` eviction.
//!
//! Run with `cargo bench --package clob`.

use clob::{Order, OrderBook, Price, Side};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

/// Mid price for the seeded book (plain units; scaling is irrelevant to
/// book mechanics).
const MID: u128 = 1_000_000;
/// Quantity resting at each level and consumed per match.
const QTY: i128 = 100;

fn make_order(id: u64, side: Side, price: u128, size: i128) -> Order {
    Order {
        id,
        side,
        price: Price::from(price),
        size,
        expiry_block: u64::MAX,
        signature: vec![],
    }
}

/// A book with `levels` single-order ask levels stacked just above mid
/// (mid+1 ..= mid+levels), ready for a Long taker to sweep.
fn seeded_ask_side(levels: usize) -> OrderBook {
    let mut book = OrderBook::new();
    for level in 0..levels {
        let id = level as u64 + 1;
        let price = MID + 1 + level as u128;
        book.process_order(make_order(id, Side::Short, price, -QTY));
    }
    book
}

fn bench_matching(c: &mut Criterion) {
    let mut group = c.benchmark_group("matching");

    // One match per iteration: the fixed cost of the matching path.
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_match", |b| {
        b.iter_batched(
            || {
                let mut book = OrderBook::new();
                book.process_order(make_order(1, Side::Short, MID, -QTY));
                (book, make_order(2, Side::Long, MID, 1))
            },
            |(mut book, taker)| {
                let results = book.process_order(taker);
                assert_eq!(results.len(), 1);
            },
            BatchSize::SmallInput,
        );
    });

    // N matches per iteration: amortized sweep cost per produced match.
    for levels in [10usize, 100] {
        group.throughput(Throughput::Elements(levels as u64));
        group.bench_function(format!("taker_sweeps_{levels}_levels"), |b| {
            b.iter_batched(
                || {
                    let book = seeded_ask_side(levels);
                    let taker = make_order(
                        u64::MAX,
                        Side::Long,
                        MID + levels as u128,
                        QTY * levels as i128,
                    );
                    (book, taker)
                },
                |(mut book, taker)| {
                    let results = book.process_order(taker);
                    assert_eq!(results.len(), levels);
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_matching);
criterion_main!(benches);
