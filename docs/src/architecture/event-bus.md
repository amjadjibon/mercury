# Event Bus

The `EventBus` is the central nervous system of Mercury. It is a thin wrapper around a crossbeam bounded channel.

## Design

- **Capacity**: 100,000 events (configurable at construction).
- **Back-pressure**: producers block when the channel is full, providing natural flow control.
- **Lock-free**: no mutex on the hot path — crossbeam's channel uses atomic operations internally.
- **Single channel**: all event types share one channel. Components filter by `EventPayload` variant.

## Event types (`EventPayload`)

| Variant | Published by | Consumed by |
|---------|-------------|-------------|
| `BookUpdate(Arc<BookUpdate>)` | FeedManager | StrategyRunner, PaperGateway |
| `Trade(Trade)` | FeedManager | StrategyRunner |
| `Signal(Signal)` | StrategyRunner | OrderManager |
| `Fill(Fill)` | OrderManager | StorageManager, StrategyRunner, IpcServer |
| `OrderUpdate(Order)` | OrderManager | IpcServer |

## Arc wrapping

`BookUpdate` is wrapped in `Arc` before publishing so that multiple subscribers can hold a reference to the same allocation without copying the order-book snapshot.

## Usage

```rust
// Publish
bus.publish(EventPayload::Signal(signal));

// Subscribe (returns a Receiver)
let rx = bus.subscribe();
while let Ok(event) = rx.recv() {
    match event.payload {
        EventPayload::Fill(fill) => { /* handle */ }
        _ => {}
    }
}
```
