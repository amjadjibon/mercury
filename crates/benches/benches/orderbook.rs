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

    BookUpdate {
        exchange: Exchange::Binance,
        symbol: Symbol::new("BTCUSDT"),
        bids,
        asks,
        sequence,
        is_snapshot: false,
    }
}

fn bench_apply_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook");
    group.throughput(Throughput::Elements(1));

    let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));

    // Initialize with snapshot
    let snapshot = BookUpdate {
        exchange: Exchange::Binance,
        symbol: Symbol::new("BTCUSDT"),
        bids: (0..100)
            .map(|i| Level::new(dec!(50000) - Decimal::from(i), dec!(1.0)))
            .collect(),
        asks: (0..100)
            .map(|i| Level::new(dec!(50001) + Decimal::from(i), dec!(1.0)))
            .collect(),
        sequence: 0,
        is_snapshot: true,
    };
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
    let snapshot = BookUpdate {
        exchange: Exchange::Binance,
        symbol: Symbol::new("BTCUSDT"),
        bids: (0..1000)
            .map(|i| Level::new(dec!(50000) - Decimal::from(i), dec!(1.0)))
            .collect(),
        asks: (0..1000)
            .map(|i| Level::new(dec!(50001) + Decimal::from(i), dec!(1.0)))
            .collect(),
        sequence: 0,
        is_snapshot: true,
    };
    book.apply_update(&snapshot);

    group.bench_function("best_bid", |b| b.iter(|| black_box(book.best_bid())));

    group.bench_function("best_ask", |b| b.iter(|| black_box(book.best_ask())));

    group.bench_function("mid_price", |b| b.iter(|| black_box(book.mid_price())));

    group.finish();
}

criterion_group!(benches, bench_apply_update, bench_best_bid_ask);
criterion_main!(benches);
