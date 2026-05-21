//! POV (Percentage of Volume) executor — participates as a fixed fraction of observed market volume.
//!
//! The executor watches `Trade` events on the event bus.  Whenever the cumulative
//! market volume since the last child order crossed `min_slice_qty`, it emits a
//! child signal sized at `market_volume * participation_rate`, subject to
//! `min_slice_qty` and `max_slice_qty` guards.
//!
//! Execution stops once the full parent quantity is filled or the optional wall-clock
//! `deadline_secs` elapses.  If the deadline fires with quantity remaining, one final
//! child is emitted for the residual.

use mercury_core::{Event, EventBus, EventPayload, FixedPoint, Signal};
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use tracing::info;

pub struct PovExecutor {
    event_bus: Arc<EventBus>,
}

impl PovExecutor {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self { event_bus }
    }

    /// Spawn a background task that participates at `participation_rate` of observed volume.
    ///
    /// # Parameters
    /// - `signal`: parent order to be worked.
    /// - `participation_rate`: fraction of market volume to participate at (e.g. `0.10` = 10%).
    /// - `min_slice_qty`: minimum child order size; accumulate volume until this threshold.
    /// - `max_slice_qty`: maximum child order size per slice.
    /// - `deadline_secs`: wall-clock time limit; remaining quantity is flushed at expiry.
    pub fn execute(
        &self,
        signal: Signal,
        participation_rate: f64,
        min_slice_qty: FixedPoint,
        max_slice_qty: FixedPoint,
        deadline_secs: u64,
    ) {
        assert!(
            participation_rate > 0.0 && participation_rate <= 1.0,
            "participation_rate must be in (0, 1]"
        );

        let bus = Arc::clone(&self.event_bus);
        let mut sub = bus.subscribe();

        tokio::spawn(async move {
            let target_symbol = signal.symbol;
            let deadline = Duration::from_secs(deadline_secs);
            let mut qty_remaining = signal.quantity;
            let mut accumulated_vol = FixedPoint::ZERO;

            // Poll volume at 100 ms granularity; flush on deadline.
            let poll_interval = Duration::from_millis(100);
            let start = tokio::time::Instant::now();

            loop {
                if qty_remaining.is_zero() {
                    break;
                }

                let elapsed = start.elapsed();
                if elapsed >= deadline {
                    // Deadline reached — flush residual.
                    emit_child(&bus, &signal, qty_remaining, accumulated_vol, true);
                    break;
                }

                // Drain new Trade events.
                accumulated_vol = FixedPoint(
                    accumulated_vol
                        .0
                        .saturating_add(drain_volume(&mut sub, target_symbol).0),
                );

                // Compute target slice size from accumulated volume.
                let target_qty = FixedPoint(
                    (accumulated_vol.0 as f64 * participation_rate) as i64,
                );

                if target_qty >= min_slice_qty {
                    let child_qty = target_qty.min(max_slice_qty).min(qty_remaining);
                    emit_child(&bus, &signal, child_qty, accumulated_vol, false);
                    qty_remaining = FixedPoint(qty_remaining.0.saturating_sub(child_qty.0));
                    accumulated_vol = FixedPoint::ZERO;
                }

                sleep(poll_interval).await;
            }
        });
    }
}

fn emit_child(
    bus: &Arc<EventBus>,
    signal: &Signal,
    child_qty: FixedPoint,
    bucket_vol: FixedPoint,
    is_final: bool,
) {
    if child_qty.is_zero() {
        return;
    }
    let child = Signal {
        quantity: child_qty,
        ..signal.clone()
    };
    info!(
        symbol = %child.symbol,
        side = ?child.side,
        qty = %child_qty,
        bucket_vol = %bucket_vol,
        is_final,
        "POV slice emitted"
    );
    let event = Event::new(bus.next_id(), EventPayload::Signal(child));
    let _ = bus.try_publish(event);
}

fn drain_volume(
    sub: &mut mercury_core::Subscriber<Event>,
    symbol: mercury_core::Symbol,
) -> FixedPoint {
    let mut vol = FixedPoint::ZERO;
    while let Ok(event) = sub.try_recv() {
        if let EventPayload::Trade(t) = event.payload {
            if t.symbol == symbol {
                vol = FixedPoint(
                    vol.0
                        .saturating_add(FixedPoint::from_decimal(t.quantity).0),
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
    async fn test_pov_deadline_flushes_residual() {
        let bus = Arc::new(EventBus::new(1_000));
        let mut sub = bus.subscribe();
        let exec = PovExecutor::new(Arc::clone(&bus));

        // No market trades injected → participation never triggers → deadline flushes all.
        let qty = dec!(2.0);
        exec.execute(
            make_signal(qty),
            0.10,
            FixedPoint::from_decimal(dec!(0.1)),
            FixedPoint::from_decimal(dec!(10.0)),
            1, // 1-second deadline
        );

        sleep(Duration::from_millis(1_300)).await;

        let mut total_qty = FixedPoint::ZERO;
        while let Ok(event) = sub.try_recv() {
            if let EventPayload::Signal(s) = event.payload {
                total_qty = FixedPoint(total_qty.0 + s.quantity.0);
            }
        }

        let expected = FixedPoint::from_decimal(qty);
        assert!(
            (total_qty.0 - expected.0).abs() <= 4,
            "deadline flush should exhaust parent qty, got {:?}",
            total_qty
        );
    }
}
