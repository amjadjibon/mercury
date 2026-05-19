//! Event bus backed by a lock-free ring buffer.

use crate::events::Event;
use crate::ring_buffer::{RingBuffer, Subscriber};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

pub use crate::ring_buffer::RecvError as EventBusError;

/// Publish-side error.
#[derive(Debug, Error)]
pub enum PublishError {
    #[error("Event bus is closed")]
    Closed,
}

/// Lock-free event bus.
///
/// All inter-component communication flows through a single pre-allocated ring
/// buffer. Every `Subscriber` tracks its own read cursor — events are written
/// once by the publisher and read independently by each subscriber (fan-out,
/// no per-subscriber copies on the publisher side).
///
/// Use `subscribe()` for both hot-path blocking consumers (strategy runner)
/// and async fan-out consumers (storage, IPC, signal handler). The returned
/// `Subscriber<Event>` supports `recv()` (blocking spin), `try_recv()`
/// (non-blocking), and `recv_async()` (async, Notify-based).
#[derive(Clone)]
pub struct EventBus {
    ring: RingBuffer<Event>,
    next_id: Arc<AtomicU64>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: RingBuffer::new(capacity),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Allocate the next monotonic event ID.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Create a subscriber whose cursor starts at the current head.
    /// All subscribers receive all events independently (fan-out semantics).
    pub fn subscribe(&self) -> Subscriber<Event> {
        self.ring.subscribe()
    }

    /// Alias for `subscribe()` — provided for backward compatibility with
    /// callers that previously used the tokio broadcast fan-out channel.
    pub fn subscribe_all(&self) -> Subscriber<Event> {
        self.ring.subscribe()
    }

    /// Publish an event. Non-blocking; always succeeds unless the bus is closed.
    pub fn try_publish(&self, event: Event) -> Result<(), PublishError> {
        self.ring.publish(event);
        Ok(())
    }

    /// Publish an event (same as `try_publish` — ring buffer never blocks).
    pub fn publish(&self, event: Event) -> Result<(), PublishError> {
        self.ring.publish(event);
        Ok(())
    }

    /// Signal shutdown. Async subscribers waiting on `recv_async` will return
    /// `RecvError::Closed` after draining any remaining events.
    pub fn close(&self) {
        self.ring.close();
    }

    pub fn capacity(&self) -> Option<usize> {
        Some(self.ring.capacity())
    }

    /// Approximate number of events published (monotonically increasing).
    pub fn len(&self) -> usize {
        self.ring.inner.publisher_seq.load(Ordering::Relaxed) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        false
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(65_536)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{BookUpdate, EventPayload};
    use crate::ring_buffer::RecvError;
    use crate::types::{Exchange, Symbol};

    #[test]
    fn test_publish_subscribe() {
        let bus = EventBus::new(128);
        let mut rx = bus.subscribe();

        let event = Event::new(
            bus.next_id(),
            EventPayload::BookUpdate(std::sync::Arc::new(BookUpdate::from_slices(
                Exchange::Binance,
                Symbol::new("BTCUSDT"),
                &[],
                &[],
                1,
                true,
            ))),
        );

        bus.publish(event.clone()).unwrap();
        let received = rx.recv().unwrap();
        assert_eq!(received.id, event.id);
    }

    #[test]
    fn test_fanout_multiple_subscribers() {
        let bus = EventBus::new(128);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        let event = Event::new(
            bus.next_id(),
            EventPayload::BookUpdate(std::sync::Arc::new(BookUpdate::from_slices(
                Exchange::Binance,
                Symbol::new("BTCUSDT"),
                &[],
                &[],
                1,
                true,
            ))),
        );

        bus.publish(event.clone()).unwrap();

        let r1 = rx1.recv().unwrap();
        let r2 = rx2.recv().unwrap();
        assert_eq!(r1.id, event.id);
        assert_eq!(r2.id, event.id);
    }

    #[test]
    fn test_lagged_subscriber() {
        let bus = EventBus::new(4); // tiny capacity: 4 slots (next power of 2)

        // Subscriber created AFTER publishing — starts at current head, not 0.
        // So we must create subscriber first, then publish past capacity.
        let mut rx = bus.subscribe();

        let make = |id| {
            Event::new(
                id,
                EventPayload::BookUpdate(std::sync::Arc::new(BookUpdate::from_slices(
                    Exchange::Binance,
                    Symbol::new("BTCUSDT"),
                    &[],
                    &[],
                    id,
                    false,
                ))),
            )
        };

        // Publish 8 events (2× capacity of 4), causing rx to lag.
        for i in 0..8 {
            bus.publish(make(i)).unwrap();
        }

        // First try_recv should return Lagged.
        match rx.try_recv() {
            Err(RecvError::Lagged(_)) => {}
            other => panic!("expected Lagged, got {other:?}"),
        }
    }
}
