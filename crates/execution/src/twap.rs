//! TWAP executor — splits a large signal into equal child slices emitted on a timer.

use mercury_core::{Event, EventBus, EventPayload, Signal};
use rust_decimal::Decimal;
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
            let child_qty = signal.quantity / Decimal::from(slices);
            // interval between slices in milliseconds; minimum 1 ms
            let interval_ms = ((duration_secs * 1_000) / slices as u64).max(1);
            let mut ticker = interval(Duration::from_millis(interval_ms));

            for i in 0..slices {
                ticker.tick().await; // first tick fires immediately
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
    use mercury_core::{EventBus, OrderType, Side, Signal, StrategyId, Symbol};
    use rust_decimal_macros::dec;
    use std::sync::Arc;
    use tokio::time::{Duration, sleep};

    fn make_signal(qty: Decimal) -> Signal {
        Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Market,
            price: None,
            quantity: qty,
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

        // wait for all slices (4 × 250 ms = 1 s + small buffer)
        sleep(Duration::from_millis(1_300)).await;

        let mut signals = 0usize;
        let mut total_qty = Decimal::ZERO;
        while let Ok(event) = sub.try_recv() {
            if let EventPayload::Signal(s) = event.payload {
                signals += 1;
                total_qty += s.quantity;
            }
        }

        assert_eq!(signals, 4, "expected 4 slices");
        // floating point: allow tiny rounding error
        assert!(
            (total_qty - dec!(1.0)).abs() < dec!(0.0001),
            "total qty should equal parent qty, got {total_qty}"
        );
    }

    #[tokio::test]
    async fn test_twap_child_qty() {
        let bus = Arc::new(EventBus::new(1_000));
        let mut sub = bus.subscribe();
        let exec = TwapExecutor::new(Arc::clone(&bus));

        exec.execute(make_signal(dec!(3.0)), 1, 3);
        sleep(Duration::from_millis(1_300)).await;

        let mut qtys: Vec<Decimal> = Vec::new();
        while let Ok(event) = sub.try_recv() {
            if let EventPayload::Signal(s) = event.payload {
                qtys.push(s.quantity);
            }
        }

        assert_eq!(qtys.len(), 3);
        for q in &qtys {
            assert_eq!(*q, dec!(1.0), "each slice should be 1.0, got {q}");
        }
    }
}
