# CLAUDE.md

This file provides guidance to Agent when working with code in this repository.

## Commands

```bash
# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Run tests for a single crate
cargo test -p mercury-core

# Run a single test by name
cargo test -p mercury-storage test_storage_manager

# Run benchmarks
cargo bench

# Run the trading engine (live)
BINANCE_API_KEY=xxx BINANCE_SECRET_KEY=yyy cargo run --bin mercury -- run --symbol BTCUSDT --strategy market_maker

# Paper trading (no real orders, 5ms simulated latency)
cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy rsi

# Run with Yahoo Finance feed (stocks)
cargo run --bin mercury -- run --symbol AAPL --exchange yahoo --strategy momentum

# Replay historical parquet data
cargo run --bin mercury -- replay --file data.parquet --speed 1.0

# Backtest a strategy
cargo run --bin mercury -- backtest --file data.parquet --strategy market_maker

# Record live market data to Parquet (default 60s)
cargo run --bin mercury -- record --symbol BTCUSDT --output data.parquet --duration 120

# Run TUI monitor
cargo run --bin mercury-tui

# Adjust log verbosity (trace, debug, info, warn, error)
cargo run --bin mercury -- --log-level debug run --symbol BTCUSDT --paper --strategy rsi

# Build and serve the mdBook documentation
mdbook serve docs --open
```

## Architecture

Mercury is an event-driven trading engine. All inter-component communication flows through a single `EventBus` (`crates/core/src/event_bus.rs`) backed by a **custom lock-free MPMC ring buffer** (`crates/core/src/ring_buffer.rs`). The ring buffer is pre-allocated with power-of-2 capacity (default 65,536 slots). Each `Subscriber` tracks its own read cursor independently — events are written once and fanned out to all subscribers with no per-subscriber copies. Slow subscribers are lossy: they skip forward and report `RecvError::Lagged`.

### Event flow

```text
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

### Key traits

| Trait | Crate | Purpose |
| --- | --- | --- |
| `Strategy` | `mercury-strategy` | Implement for new strategies. Methods: `on_book(&OrderBook)`, `on_trade`, `on_fill`, `on_sentiment`, `reset`. |
| `ExchangeGateway` | `mercury-gateway` | Implement for new exchanges. Async: `submit_order`, `cancel_order`, `connect`. |
| `FeedParser` | `mercury-market` | Implement for new exchange WebSocket feeds. Methods: `parse`, `ws_url`, `subscribe_message`. |

### Crate dependency order

`core` ← `market`, `gateway`, `strategy`, `risk`, `execution`, `replay`, `metrics`, `storage` ← `cli`, `tui`

- `core`: `Event`, `EventBus`, `RingBuffer`, `OrderBook`, `Pool`, `IpcServer`, all shared types (`Symbol`, `Exchange`, `Side`, `Order`, `Fill`, `Signal`, `StrategyId`, etc.)
- `market`: `FeedManager`, `BinanceParser`, `CoinbaseParser`, `YahooFeed`, `BookBuilder`
- `gateway`: `BinanceGateway`, `BybitGateway`, `CoinbaseGateway`, `KrakenGateway`, `OkxGateway`, `PaperGateway`
- `strategy`: `StrategyRunner`, `MarketMaker`, `Momentum`, `RsiStrategy`, `ArbitrageStrategy`, `InferenceStrategy`, `RlQuotePlacementStrategy`, `PairsStrategy`, `ObiStrategy`, `TriangularStrategy`, `SentimentStrategy`, plus indicators (`Sma`, `Ema`, `Rsi`, `Macd`, `Atr`)
- `risk`: `RiskManager` (position limits, daily loss guard, rate limiting, kill switch)
- `execution`: `OrderManager`, `SimulatedExchange` (for backtest), `SmartOrderRouter`, `ExecutionMetrics`
- `replay`: `Recorder` (writes ticks to Parquet), `Player` (deterministic replay)
- `metrics`: `LatencyTracker` (HDR histograms), `PnlTracker`, Prometheus export
- `storage`: `StorageManager` (SQLite via sqlx; migrations in `crates/storage/migrations/`)
- `cli`: Binary `mercury` — `run`, `replay`, `backtest`, `record` subcommands
- `tui`: Binary `mercury-tui` — ratatui dashboard over `/tmp/mercury.sock`

### EventPayload variants

`BookUpdate(Arc<BookUpdate>)`, `Trade`, `Signal`, `Order`, `Fill`, `RiskAlert`, `LatencyReport`, `MLPrediction`, `SentimentSignal`

### StrategyRunner and OrderBook

`StrategyRunner` maintains a `HashMap<Symbol, OrderBook>` and applies each `BookUpdate` before calling strategies. Strategies receive `&OrderBook` (the fully-reconstructed book), not the raw `BookUpdate`. This means `on_book` always sees consistent L2 state.

### StrategyId

`StrategyId` is a `#[repr(u8)]` enum — all 10 built-in strategies have a fixed discriminant. `Signal` carries a `StrategyId` field so it is fully stack-allocated with no heap allocation.

### Decimal arithmetic

All prices and quantities use `rust_decimal::Decimal` via the `FixedPoint` type alias. Use `rust_decimal_macros::dec!()` for literals (e.g., `dec!(0.01)`). Do not use `f64` for financial values.

### Symbol type

`Symbol` is a fixed-size `[u8; 16]` array (stack-allocated, no heap). Symbol strings are silently truncated to 16 bytes. Use `Symbol::new("BTCUSDT")` or `"BTCUSDT".into()`.

### Concurrency model

- Hot path is lock-free: `EventBus` ring buffer uses atomic sequence numbers; no mutex on publish/receive.
- Long-running components are each spawned as `tokio::spawn` tasks.
- `StrategyRunner` owns all strategies and calls them synchronously in a single task — `on_book`/`on_trade` take `&mut self` and must not block.

### Paper trading

`PaperGateway` (`crates/gateway/src/paper.rs`) matches orders against live `BookUpdate` events on the bus. Configurable RTT latency (default 5 ms). Use `--paper` flag; no API keys needed.

### Storage

SQLite database (`mercury.db` by default). Schema managed with sqlx migrations (`crates/storage/migrations/`). Currently only `Fill` events are persisted to a `trades` table. Use `:memory:` in tests.
