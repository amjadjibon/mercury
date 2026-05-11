# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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
```

## Architecture

Mercury is an event-driven trading engine. All inter-component communication flows through a single `EventBus` (crossbeam bounded channel, default capacity 100,000). Components publish and subscribe to `Event` values containing an `EventPayload` enum variant.

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
| `Strategy` | `mercury-strategy` | Implement for new trading strategies. Methods: `on_book`, `on_trade`, `on_fill`, `reset`. |
| `ExchangeGateway` | `mercury-gateway` | Implement for new exchanges. Async: `submit_order`, `cancel_order`, `connect`. |
| `FeedParser` | `mercury-market` | Implement for new exchange WebSocket feeds. Methods: `parse`, `ws_url`, `subscribe_message`. |

### Crate dependency order

`core` ← `market`, `gateway`, `strategy`, `risk`, `execution`, `replay`, `metrics`, `storage` ← `cli`, `tui`

- `core`: `Event`, `EventBus`, `OrderBook`, `Pool`, `IpcServer`, all shared types (`Symbol`, `Exchange`, `Side`, `Order`, `Fill`, `Signal`, etc.)
- `market`: `FeedManager` (manages WebSocket connections), `BinanceParser`, `CoinbaseParser`, `YahooFeed`, `BookBuilder`
- `gateway`: `BinanceGateway`, `CoinbaseGateway`, `PaperGateway` (local matching engine with configurable latency)
- `strategy`: `StrategyRunner` (dispatches events to strategies), `MarketMaker`, `Momentum`, `RsiStrategy`, `ArbitrageStrategy`, `InferenceStrategy` (ML-based), technical indicators (`SMA`, `EMA`, `RSI`, `MACD`)
- `risk`: `RiskManager` (position limits, daily loss guard, rate limiting, kill switch)
- `execution`: `OrderManager` (wraps gateway with risk checks), `SimulatedExchange` (for backtest), `SmartOrderRouter` (best-execution across venues), `ExecutionMetrics`
- `replay`: `Recorder` (writes ticks to Parquet), `Player` (deterministic replay)
- `metrics`: `LatencyTracker` (HDR histograms), `PnlTracker`, Prometheus export
- `storage`: `StorageManager` (SQLite via sqlx, persists fills; migrations in `crates/storage/migrations/`)
- `cli`: Binary `mercury` — `run`, `replay`, `backtest`, `record` subcommands
- `tui`: Binary `mercury-tui` — connects via `/tmp/mercury.sock` IPC, ratatui dashboard

### Decimal arithmetic

All prices and quantities use `rust_decimal::Decimal`. Use `rust_decimal_macros::dec!()` for literals (e.g., `dec!(0.01)`). Do not use `f64` for financial values.

### Symbol type

`Symbol` is a fixed-size `[u8; 16]` array (stack-allocated, no heap). Symbol strings are silently truncated to 16 bytes. Use `Symbol::new("BTCUSDT")` or `"BTCUSDT".into()`.

### Concurrency model

- Hot path is lock-free: `EventBus` uses crossbeam bounded channels; `Pool` uses `parking_lot`.
- Long-running components (feed manager, strategy runner, fill handler, storage writer, IPC broadcaster) are each spawned as `tokio::spawn` tasks.
- `Strategy` trait is `Send + Sync` but `on_book`/`on_trade` take `&mut self` — `StrategyRunner` owns strategies and runs them synchronously in a single thread.

### Paper trading

`PaperGateway` (`crates/gateway/src/paper.rs`) matches orders against live `BookUpdate` events on the bus. Configurable RTT latency (default 5 ms). Use `--paper` flag; no API keys needed.

### Storage

SQLite database (`mercury.db` by default). Schema managed with sqlx migrations (`crates/storage/migrations/`). Currently only `Fill` events are persisted to a `trades` table. Use `:memory:` in tests.
