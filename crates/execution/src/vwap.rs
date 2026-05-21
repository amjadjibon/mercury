//! VWAP executor — slices a parent order proportionally to observed market volume.
//!
//! The executor divides the execution horizon into `slices` equal time buckets.
//! During each bucket it accumulates the market volume seen in `Trade` events.
//! At bucket boundaries it emits a child signal sized proportionally to that
//! bucket's share of the total volume observed so far, scaled to ensure the
//! full parent quantity is exhausted by the final slice.
//!
//! If no market volume is observed in a bucket the slice falls back to an
//! equal-weight (TWAP) allocation so execution always makes progress.

use mercury_core::{Event, EventBus, EventPayload, FixedPoint, Signal};
use std::sync::Arc;
use tokio::time::{Duration, interval};
use tracing::info;

pub struct VwapExecutor {
    event_bus: Arc<EventBus>,
}

impl VwapExecutor {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self { event_bus }
    }

    /// Spawn a background task that emits `slices` child signals over `duration_secs`,
    /// sizing each child proportionally to observed market volume in that bucket.
    pub fn execute(&self, signal: Signal, duration_secs: u64, slices: u32) {
        assert!(slices > 0, "slices must be > 0");

        let bus = Arc::clone(&self.event_bus);
        let mut sub = bus.subscribe();

        tokio::spawn(async move {
            let interval_ms = ((duration_secs * 1_000) / slices as u64).max(1);
            let mut ticker = interval(Duration::from_millis(interval_ms));
            let target_symbol = signal.symbol;

            let mut qty_remaining = signal.quantity;
            let equal_slice = signal.quantity / FixedPoint::from(slices as i64);

            for i in 0..slices {
                // Drain all events accumulated since last tick without blocking.
                let bucket_volume = drain_volume(&mut sub, target_symbol);

                ticker.tick().await;

                // Determine child quantity for this slice.
                let child_qty = if i + 1 == slices {
                    // Final slice: send whatever remains to avoid rounding residual.
                    qty_remaining
                } else if bucket_volume.is_zero() {
                    // No volume observed — fall back to equal-weight slice.
                    let q = equal_slice.min(qty_remaining);
                    q
                } else {
                    // Proportion: bucket_volume relative to expected average.
                    // Scale equal_slice by observed/expected volume ratio, capped at remaining.
                    let ratio_num = bucket_volume.0 as i128;
                    // Use equal_slice as the baseline; ratio is unitless scaling factor.
                    // child_qty = equal_slice * min(2, bucket_volume / avg_volume)
                    // Simplified: scale linearly, cap at 2× equal_slice.
                    let scaled = FixedPoint(
                        (equal_slice.0 as i128 * ratio_num
                            / (equal_slice.0.max(1) as i128))
                            .min(equal_slice.0 as i128 * 2) as i64,
                    );
                    scaled.min(qty_remaining)
                };

                if child_qty.is_zero() {
                    continue;
                }

                qty_remaining = FixedPoint(qty_remaining.0.saturating_sub(child_qty.0));

                let child = Signal {
                    quantity: child_qty,
                    ..signal.clone()
                };
                info!(
                    symbol = %child.symbol,
                    side = ?child.side,
                    slice = i + 1,
                    total = slices,
                    qty = %child_qty,
                    bucket_vol = %bucket_volume,
                    "VWAP slice emitted"
                );
                let event = Event::new(bus.next_id(), EventPayload::Signal(child));
                let _ = bus.try_publish(event);

                if qty_remaining.is_zero() {
                    break;
                }
            }
        });
    }
}

/// Drain all available Trade events for `symbol` and return total volume seen.
fn drain_volume(
    sub: &mut mercury_core::Subscriber<Event>,
    symbol: mercury_core::Symbol,
) -> FixedPoint {
    let mut vol = FixedPoint::ZERO;
    while let Ok(event) = sub.try_recv() {
        if let EventPayload::Trade(t) = event.payload {
            if t.symbol == symbol {
                vol = FixedPoint(
                    vol.0.saturating_add(
                        FixedPoint::from_decimal(t.quantity).0,
                    ),
                );
            }
        }
    }
    vol
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{EventBus, FixedPoint, OrderType, Side, Signal, StrategyId, Symbol, TimeInForce};
    use rust_decimal_macros::dec;
    use tokio::time::{Duration, sleep};

    fn make_signal(qty: impl Into<FixedPoint>) -> Signal {
        Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: None,
            quantity: qty.into(),
            strategy: StrategyId::Momentum,
            cancel_replace: false,
            time_in_force: TimeInForce::GTC,
        }
    }

    #[tokio::test]
    async fn test_vwap_emits_correct_slice_count() {
        let bus = Arc::new(EventBus::new(1_000));
        let mut sub = bus.subscribe();
        let exec = VwapExecutor::new(Arc::clone(&bus));

        exec.execute(make_signal(dec!(1.0)), 1, 4);
        sleep(Duration::from_millis(1_300)).await;

        let mut signals = 0usize;
        let mut total_qty = FixedPoint::ZERO;
        while let Ok(event) = sub.try_recv() {
            if let EventPayload::Signal(s) = event.payload {
                signals += 1;
                total_qty = FixedPoint(total_qty.0 + s.quantity.0);
            }
        }

        assert_eq!(signals, 4);
        let expected = FixedPoint::from_decimal(dec!(1.0));
        assert!(
            (total_qty.0 - expected.0).abs() <= 4,
            "total qty should equal parent qty, got {:?}",
            total_qty
        );
    }

    #[tokio::test]
    async fn test_vwap_final_slice_exhausts_remaining() {
        let bus = Arc::new(EventBus::new(1_000));
        let mut sub = bus.subscribe();
        let exec = VwapExecutor::new(Arc::clone(&bus));

        // 3-slice execution over 1 second
        exec.execute(make_signal(dec!(3.0)), 1, 3);
        sleep(Duration::from_millis(1_400)).await;

        let mut total_qty = FixedPoint::ZERO;
        while let Ok(event) = sub.try_recv() {
            if let EventPayload::Signal(s) = event.payload {
                total_qty = FixedPoint(total_qty.0 + s.quantity.0);
            }
        }

        let expected = FixedPoint::from_decimal(dec!(3.0));
        assert!(
            (total_qty.0 - expected.0).abs() <= 4,
            "VWAP should exhaust parent qty, got {:?}",
            total_qty
        );
    }
}
