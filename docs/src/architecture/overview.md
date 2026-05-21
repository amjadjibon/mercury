# Architecture Overview

Mercury is an event-driven trading engine. All inter-component communication flows through a single `EventBus` — a crossbeam bounded channel with a default capacity of 100,000 events.

## Event flow

```
Market Feeds (WebSocket) → FeedManager → EventBus
                                              │
             ┌────────────────────────────────┼──────────────────┐
             ▼                                ▼                  ▼
      StrategyRunner                    OrderManager         StorageManager
      (on_book/on_trade)               (submits orders       (persists fills
             │                          via Gateway)          to SQLite)
             ▼                                │
         Signal events                    Fill events
         → EventBus                       → EventBus
                                              │
                                         IpcServer
                                    (/tmp/mercury.sock)
                                    (broadcasts to TUI)
```

## Key traits

| Trait | Crate | Purpose |
|-------|-------|---------|
| `Strategy` | `mercury-strategy` | Implement for new trading strategies. Methods: `on_book`, `on_trade`, `on_fill`, `reset`. |
| `ExchangeGateway` | `mercury-gateway` | Implement for new exchanges. Async: `submit_order`, `cancel_order`, `connect`. |
| `FeedParser` | `mercury-market` | Implement for new exchange WebSocket feeds. Methods: `parse`, `ws_url`, `subscribe_message`. |

## Component lifecycle

Each long-running component is spawned as a `tokio::spawn` task:

1. **FeedManager** — opens WebSocket connections, parses messages, publishes `BookUpdate` and `Trade` events.
2. **StrategyRunner** — subscribes to market events, calls strategy methods, publishes `Signal` events.
3. **OrderManager** — subscribes to `Signal` events, applies risk checks, submits orders via the gateway.
4. **StorageManager** — subscribes to `Fill` events and persists them to SQLite.
5. **IpcServer** — fans out events to connected TUI clients over a Unix socket.
