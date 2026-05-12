//! Order book benchmarks.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use mercury_core::{BookUpdate, Exchange, Level, OrderBook, Symbol};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn create_update(sequence: u64, num_levels: usize) -> BookUpdate {
    let bids: Vec<Level> = (0..num_levels)
        .map(|i| Level::new(dec!(50000) - Decimal::from(i), dec!(1.0)))
        .collect();

    let asks: Vec<Level> = (0..num_levels)
        .map(|i| Level::new(dec!(50001) + Decimal::from(i), dec!(1.0)))
        .collect();

    BookUpdate::from_slices(
        Exchange::Binance,
        Symbol::new("BTCUSDT"),
        &bids,
        &asks,
        sequence,
        false,
    )
}

fn bench_apply_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook");
    group.throughput(Throughput::Elements(1));

    let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));

    // Initialize with snapshot (clamped to MAX_LEVELS=20)
    let bids: Vec<Level> = (0..20)
        .map(|i| Level::new(dec!(50000) - Decimal::from(i), dec!(1.0)))
        .collect();
    let asks: Vec<Level> = (0..20)
        .map(|i| Level::new(dec!(50001) + Decimal::from(i), dec!(1.0)))
        .collect();
    let snapshot = BookUpdate::from_slices(
        Exchange::Binance,
        Symbol::new("BTCUSDT"),
        &bids,
        &asks,
        0,
        true,
    );
    book.apply_update(&snapshot);

    group.bench_function("apply_10_levels", |b| {
        let mut seq = 1u64;
        b.iter(|| {
            seq += 1;
            let update = create_update(seq, 10);
            book.apply_update(black_box(&update));
        })
    });

    group.finish();
}

fn bench_best_bid_ask(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook");
    group.throughput(Throughput::Elements(1));

    let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
    // Seed book with 20 levels (MAX_LEVELS)
    let bids: Vec<Level> = (0..20)
        .map(|i| Level::new(dec!(50000) - Decimal::from(i), dec!(1.0)))
        .collect();
    let asks: Vec<Level> = (0..20)
        .map(|i| Level::new(dec!(50001) + Decimal::from(i), dec!(1.0)))
        .collect();
    let snapshot = BookUpdate::from_slices(
        Exchange::Binance,
        Symbol::new("BTCUSDT"),
        &bids,
        &asks,
        0,
        true,
    );
    book.apply_update(&snapshot);

    group.bench_function("best_bid", |b| b.iter(|| black_box(book.best_bid())));

    group.bench_function("best_ask", |b| b.iter(|| black_box(book.best_ask())));

    group.bench_function("mid_price", |b| b.iter(|| black_box(book.mid_price())));

    group.finish();
}

criterion_group!(benches, bench_apply_update, bench_best_bid_ask);
criterion_main!(benches);
