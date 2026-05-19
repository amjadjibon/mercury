//! Event bus benchmarks.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use mercury_core::{BookUpdate, Event, EventBus, EventPayload, Exchange, Level, Symbol};
use rust_decimal_macros::dec;

fn create_event(id: u64) -> Event {
    Event::new(
        id,
        EventPayload::BookUpdate(std::sync::Arc::new(BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(dec!(50000), dec!(1.0))],
            &[Level::new(dec!(50001), dec!(1.0))],
            id,
            false,
        ))),
    )
}

fn bench_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_bus");
    group.throughput(Throughput::Elements(1));

    let bus = EventBus::new(1_000_000);
    let mut rx = bus.subscribe();

    // Drain in background to prevent the ring buffer from lapping.
    std::thread::spawn(move || while rx.recv().is_some() {});

    group.bench_function("publish", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let event = create_event(id);
            bus.try_publish(black_box(event)).unwrap();
        })
    });

    group.finish();
}

fn bench_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_bus");
    group.throughput(Throughput::Elements(1));

    let bus = EventBus::new(1_000_000);
    let mut rx = bus.subscribe();

    group.bench_function("roundtrip", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let event = create_event(id);
            bus.try_publish(event).unwrap();
            let _ = black_box(rx.try_recv());
        })
    });

    group.finish();
}

criterion_group!(benches, bench_publish, bench_roundtrip);
criterion_main!(benches);
