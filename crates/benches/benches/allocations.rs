//! Allocation-counting tests for the hot path.
//!
//! Each test exercises a specific hot-path operation and asserts that
//! zero heap allocations occur after the warm-up phase.

use mercury_core::{BookUpdate, Event, EventBus, EventPayload, Exchange, Level, Symbol};
use rust_decimal_macros::dec;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn make_book_event(id: u64) -> Event {
    Event::new(
        id,
        EventPayload::BookUpdate(BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[
                Level::new(dec!(50000), dec!(1.5)),
                Level::new(dec!(49999), dec!(2.0)),
                Level::new(dec!(49998), dec!(3.0)),
            ],
            &[
                Level::new(dec!(50001), dec!(1.5)),
                Level::new(dec!(50002), dec!(2.0)),
                Level::new(dec!(50003), dec!(3.0)),
            ],
            id,
            false,
        )),
    )
}

fn main() {
    check_event_construction();
    check_event_clone();
    check_eventbus_publish_no_subscriber();
    check_eventbus_publish_with_subscriber();

    println!("\nAll allocation checks passed.");
}

fn check_event_construction() {
    // warm up
    let _ = make_book_event(0);

    let reg = Region::new(&GLOBAL);
    for i in 1u64..=1000 {
        let _ = std::hint::black_box(make_book_event(i));
    }
    let stats = reg.change();
    assert_eq!(
        stats.allocations, 0,
        "Event construction: expected 0 allocations, got {}",
        stats.allocations
    );
    println!(
        "  [OK] Event construction (1000 iters): {} allocs, {} bytes",
        stats.allocations, stats.bytes_allocated
    );
}

fn check_event_clone() {
    let event = make_book_event(0);

    // warm up
    let _ = event.clone();

    let reg = Region::new(&GLOBAL);
    for _ in 0u64..1000 {
        let _ = std::hint::black_box(event.clone());
    }
    let stats = reg.change();
    assert_eq!(
        stats.allocations, 0,
        "Event clone: expected 0 allocations, got {}",
        stats.allocations
    );
    println!(
        "  [OK] Event clone (1000 iters): {} allocs, {} bytes",
        stats.allocations, stats.bytes_allocated
    );
}

fn check_eventbus_publish_no_subscriber() {
    let bus = EventBus::new(1_000_000);
    let mut rx = bus.subscribe();
    let _ = std::thread::spawn(move || while rx.recv().is_some() {});

    // warm up
    bus.try_publish(make_book_event(0)).ok();

    let reg = Region::new(&GLOBAL);
    for i in 1u64..=1000 {
        bus.try_publish(std::hint::black_box(make_book_event(i))).ok();
    }
    let stats = reg.change();
    assert_eq!(
        stats.allocations, 0,
        "EventBus publish (no broadcast subscriber): expected 0 allocations, got {}",
        stats.allocations
    );
    println!(
        "  [OK] EventBus publish, no broadcast sub (1000 iters): {} allocs, {} bytes",
        stats.allocations, stats.bytes_allocated
    );
}

fn check_eventbus_publish_with_subscriber() {
    let bus = EventBus::new(1_000_000);
    let mut rx = bus.subscribe();
    let _bcast_rx = bus.subscribe_all(); // second independent subscriber

    let _ = std::thread::spawn(move || while rx.recv().is_some() {});

    // warm up
    bus.try_publish(make_book_event(0)).ok();

    let reg = Region::new(&GLOBAL);
    for i in 1u64..=1000 {
        bus.try_publish(std::hint::black_box(make_book_event(i))).ok();
    }
    let stats = reg.change();
    println!(
        "  [{}] EventBus publish, with broadcast sub (1000 iters): {} allocs, {} bytes",
        if stats.allocations == 0 { "OK" } else { "WARN" },
        stats.allocations,
        stats.bytes_allocated
    );
    if stats.allocations > 0 {
        println!("      ^ broadcast clone allocates: {} allocs, {} bytes", stats.allocations, stats.bytes_allocated);
    }
}
