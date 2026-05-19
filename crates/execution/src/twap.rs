//! TWAP executor — splits a large signal into equal child slices emitted on a timer.

use mercury_core::{Event, EventBus, EventPayload, FixedPoint, Signal};
use std::sync::Arc;
use tokio::time::{Duration, interval};
use tracing::info;

/// Splits a parent signal into `slices` child signals emitted uniformly over `duration_secs`.
///
/// Each child gets `quantity / slices` of the parent quantity.  The first slice fires
/// immediately; subsequent slices fire every `duration_secs / slices` seconds after that.
pub struct TwapExecutor {
    event_bus: Arc<EventBus>,
}

impl TwapExecutor {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self { event_bus }
    }

    /// Spawn a background task that emits `slices` child signals over `duration_secs`.
    ///
    /// Returns immediately; the task runs concurrently via tokio.
    pub fn execute(&self, signal: Signal, duration_secs: u64, slices: u32) {
        assert!(slices > 0, "slices must be > 0");
        let bus = Arc::clone(&self.event_bus);
        tokio::spawn(async move {
            let child_qty = signal.quantity / FixedPoint::from(slices as i64);
            let interval_ms = ((duration_secs * 1_000) / slices as u64).max(1);
            let mut ticker = interval(Duration::from_millis(interval_ms));

            for i in 0..slices {
                ticker.tick().await;
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
                    "TWAP slice emitted"
                );
                let event = Event::new(bus.next_id(), EventPayload::Signal(child));
                let _ = bus.try_publish(event);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{EventBus, FixedPoint, OrderType, Side, Signal, StrategyId, Symbol};
    use rust_decimal_macros::dec;
    use std::sync::Arc;
    use tokio::time::{Duration, sleep};

    fn make_signal(qty: impl Into<FixedPoint>) -> Signal {
        Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Market,
            price: None,
            quantity: qty.into(),
            strategy: StrategyId::Momentum,
            cancel_replace: false,
        }
    }

    #[tokio::test]
    async fn test_twap_emits_correct_slice_count() {
        let bus = Arc::new(EventBus::new(1_000));
        let mut sub = bus.subscribe();
        let exec = TwapExecutor::new(Arc::clone(&bus));

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

        assert_eq!(signals, 4, "expected 4 slices");
        // FixedPoint division is exact for powers of 10; allow ±1 raw unit rounding
        let expected = FixedPoint::from_decimal(dec!(1.0));
        assert!(
            (total_qty.0 - expected.0).abs() <= 4,
            "total qty should equal parent qty, got {:?}", total_qty
        );
    }

    #[tokio::test]
    async fn test_twap_child_qty() {
        let bus = Arc::new(EventBus::new(1_000));
        let mut sub = bus.subscribe();
        let exec = TwapExecutor::new(Arc::clone(&bus));

        exec.execute(make_signal(dec!(3.0)), 1, 3);
        sleep(Duration::from_millis(1_300)).await;

        let mut qtys: Vec<FixedPoint> = Vec::new();
        while let Ok(event) = sub.try_recv() {
            if let EventPayload::Signal(s) = event.payload {
                qtys.push(s.quantity);
            }
        }

        assert_eq!(qtys.len(), 3);
        let expected = FixedPoint::from_decimal(dec!(1.0));
        for q in &qtys {
            assert_eq!(*q, expected, "each slice should be 1.0, got {:?}", q);
        }
    }
}
