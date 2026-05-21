# Concurrency Model

## Hot path is lock-free

- `EventBus` uses crossbeam bounded channels — no mutex on publish/receive.
- `Pool` uses `parking_lot::Mutex`, which is fair and low-overhead.
- `Symbol` is a `[u8; 16]` stack array — no heap allocation on the critical path.

## Task topology

Each long-running component runs as an independent `tokio::spawn` task:

| Task | Notes |
|------|-------|
| FeedManager | One task per exchange connection. Handles WebSocket reconnection. |
| StrategyRunner | Single-threaded. Owns all strategy instances. Calls `on_book`/`on_trade` synchronously. |
| OrderManager | Awaits signals; submits orders to the gateway asynchronously. |
| StorageManager | Batches fill writes to SQLite. |
| IpcServer | Fans out to connected TUI clients. Non-blocking; slow clients are dropped. |

## Strategy thread safety

`Strategy` is `Send + Sync`, but `on_book` and `on_trade` take `&mut self`. `StrategyRunner` owns the strategies and calls them from a single Tokio task — no concurrent mutation, no locking needed inside strategy implementations.

## Back-pressure

The `EventBus` channel has a bounded capacity (default 100,000). If a slow consumer (e.g. StorageManager during a disk flush) falls behind, producers will block. This prevents unbounded memory growth but means the market feed task may stall. Size the channel according to your expected burst rate.
