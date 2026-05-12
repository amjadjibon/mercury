//! Strategy runner benchmarks.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use mercury_core::{BookUpdate, Event, EventBus, EventPayload, Exchange, Level, OrderBook, Symbol};
use mercury_strategy::{MarketMaker, Strategy, StrategyRunner};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;

fn make_book_event(seq: u64) -> Event {
    Event::new(
        seq,
        EventPayload::BookUpdate(BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[
                Level::new(dec!(80000) - Decimal::from(seq % 10), dec!(1.5)),
                Level::new(dec!(79999) - Decimal::from(seq % 10), dec!(2.0)),
            ],
            &[
                Level::new(dec!(80001) + Decimal::from(seq % 10), dec!(1.2)),
                Level::new(dec!(80002) + Decimal::from(seq % 10), dec!(0.8)),
            ],
            seq,
            false,
        )),
    )
}

fn bench_strategy_process(c: &mut Criterion) {
    let mut group = c.benchmark_group("strategy");
    group.throughput(Throughput::Elements(1));

    let bus = Arc::new(EventBus::new(1_000_000));
    let mut runner = StrategyRunner::new(Arc::clone(&bus));
    runner.add_strategy(Box::new(MarketMaker::new(10, dec!(0.01), dec!(1.0))));

    // Pre-warm the order book with a snapshot
    let snapshot = Event::new(
        0,
        EventPayload::BookUpdate(BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(dec!(80000), dec!(1.0))],
            &[Level::new(dec!(80001), dec!(1.0))],
            0,
            true,
        )),
    );
    runner.process(&snapshot);

    group.bench_function("process_book_update", |b| {
        let mut seq = 1u64;
        b.iter(|| {
            seq += 1;
            let event = make_book_event(seq);
            black_box(runner.process(&event));
        })
    });

    group.finish();
}

fn bench_orderbook_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("strategy");
    group.throughput(Throughput::Elements(1));

    let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
    let snapshot = BookUpdate::from_slices(
        Exchange::Binance,
        Symbol::new("BTCUSDT"),
        &[Level::new(dec!(80000), dec!(1.0))],
        &[Level::new(dec!(80001), dec!(1.0))],
        0,
        true,
    );
    book.apply_update(&snapshot);

    let mut mm = MarketMaker::new(10, dec!(0.01), dec!(1.0));

    group.bench_function("market_maker_on_book", |b| {
        b.iter(|| {
            black_box(mm.on_book(&book));
        })
    });

    group.finish();
}

criterion_group!(benches, bench_strategy_process, bench_orderbook_direct);
criterion_main!(benches);
