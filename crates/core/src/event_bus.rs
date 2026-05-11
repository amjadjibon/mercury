//! Lock-free event bus using crossbeam channels.

use crate::events::Event;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tokio::sync::broadcast;

/// Event bus errors.
#[derive(Debug, Error)]
pub enum EventBusError {
    #[error("Event bus is full")]
    Full,
    #[error("Event bus is disconnected")]
    Disconnected,
}

impl<T> From<TrySendError<T>> for EventBusError {
    fn from(err: TrySendError<T>) -> Self {
        match err {
            TrySendError::Full(_) => EventBusError::Full,
            TrySendError::Disconnected(_) => EventBusError::Disconnected,
        }
    }
}

/// Lock-free event bus for high-throughput message passing.
///
/// Crossbeam bounded channel serves the hot-path single consumer (StrategyRunner).
/// Tokio broadcast channel fans out to all observers (IPC, Storage, etc.) so every
/// subscriber receives every event without competing.
#[derive(Clone)]
pub struct EventBus {
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    broadcast_tx: broadcast::Sender<Event>,
    next_id: std::sync::Arc<AtomicU64>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = bounded(capacity);
        let (broadcast_tx, _) = broadcast::channel(capacity.min(65536));
        Self {
            sender,
            receiver,
            broadcast_tx,
            next_id: std::sync::Arc::new(AtomicU64::new(1)),
        }
    }

    /// Get the next event ID.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Publish an event to the bus.
    ///
    /// Sends to both the hot-path crossbeam channel and the broadcast fan-out.
    /// Returns immediately if the crossbeam channel is full (non-blocking).
    pub fn try_publish(&self, event: Event) -> Result<(), EventBusError> {
        let _ = self.broadcast_tx.send(event.clone());
        self.sender.try_send(event)?;
        Ok(())
    }

    /// Publish an event, blocking if the crossbeam channel is full.
    pub fn publish(&self, event: Event) -> Result<(), EventBusError> {
        let _ = self.broadcast_tx.send(event.clone());
        self.sender
            .send(event)
            .map_err(|_| EventBusError::Disconnected)
    }

    /// Subscribe via crossbeam (MPMC work-stealing). Use only for the single
    /// primary hot-path consumer (StrategyRunner). Each event goes to one receiver.
    pub fn subscribe(&self) -> Receiver<Event> {
        self.receiver.clone()
    }

    /// Subscribe to all events via tokio broadcast fan-out. Every subscriber
    /// receives every event independently. Use for IPC, storage, signal handlers.
    pub fn subscribe_all(&self) -> broadcast::Receiver<Event> {
        self.broadcast_tx.subscribe()
    }

    /// Try to receive an event without blocking.
    pub fn try_recv(&self) -> Option<Event> {
        self.receiver.try_recv().ok()
    }

    /// Receive an event, blocking until one is available.
    pub fn recv(&self) -> Option<Event> {
        self.receiver.recv().ok()
    }

    /// Get the number of events currently in the bus.
    pub fn len(&self) -> usize {
        self.sender.len()
    }

    /// Check if the bus is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the capacity of the bus.
    pub fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }

    /// Check if the bus is full.
    pub fn is_full(&self) -> bool {
        self.sender.is_full()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(100_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::BookUpdate;
    use crate::events::EventPayload;
    use crate::types::{Exchange, Symbol};

    #[test]
    fn test_publish_subscribe() {
        let bus = EventBus::new(100);
        let receiver = bus.subscribe();

        let event = Event::new(
            bus.next_id(),
            EventPayload::BookUpdate(BookUpdate {
                exchange: Exchange::Binance,
                symbol: Symbol::new("BTCUSDT"),
                bids: vec![],
                asks: vec![],
                sequence: 1,
                is_snapshot: true,
            }),
        );

        bus.publish(event.clone()).unwrap();
        let received = receiver.recv().unwrap();
        assert_eq!(received.id, event.id);
    }

    #[test]
    fn test_try_publish_full() {
        let bus = EventBus::new(1);

        let event = Event::new(
            1,
            EventPayload::BookUpdate(BookUpdate {
                exchange: Exchange::Binance,
                symbol: Symbol::new("BTCUSDT"),
                bids: vec![],
                asks: vec![],
                sequence: 1,
                is_snapshot: true,
            }),
        );

        bus.try_publish(event.clone()).unwrap();
        let result = bus.try_publish(event);
        assert!(matches!(result, Err(EventBusError::Full)));
    }

    #[test]
    fn test_multiple_subscribers() {
        let bus = EventBus::new(100);
        let _rx1 = bus.subscribe();
        let _rx2 = bus.subscribe();

        // All subscribers share the same receiver
        // (crossbeam bounded channel is MPMC)
    }
}
