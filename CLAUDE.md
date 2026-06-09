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

# Run benchmarks (includes a zero-heap-alloc assertion on the hot path)
cargo bench

# Paper trading — no API keys, matches against live Binance feed
cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy market_maker

# Live trading
BINANCE_API_KEY=xxx BINANCE_SECRET_KEY=yyy cargo run --bin mercury -- run --symbol BTCUSDT --strategy market_maker

# Yahoo Finance (stocks)
cargo run --bin mercury -- run --symbol AAPL --exchange yahoo --strategy momentum

# Record live ticks to Parquet
cargo run --bin mercury -- record --symbol BTCUSDT --output data.parquet --duration 120

# Backtest / replay recorded data
cargo run --bin mercury -- backtest --file data.parquet --strategy market_maker
cargo run --bin mercury -- replay --file data.parquet --speed 1.0

# TUI monitor (connects to running engine over Unix socket)
cargo run --bin mercury-tui

# Desktop GUI (Iced, GPU-accelerated)
cargo run -p mercury-gui

# Adjust log verbosity
cargo run --bin mercury -- --log-level debug run --symbol BTCUSDT --paper --strategy rsi

# Build and serve mdBook docs
mdbook serve docs --open
```

## CLI Reference

```
mercury run      --symbol <SYM>
                 [--exchange binance|coinbase|bybit|kraken|okx|yahoo|polymarket|kalshi]
                 [--strategy market_maker|momentum|rsi|arbitrage|inference|pairs|obi|triangular|sentiment|rl]
                 [--paper]
                 [--api-key <KEY>] [--secret-key <SECRET>]
                 [--okx-passphrase <PASS>]
                 [--polymarket-passphrase <PASS>]
                 [--kalshi-api-key-id <UUID>] [--kalshi-private-key <PEM|PATH>]
                 [--log-level trace|debug|info|warn|error]

mercury record   --symbol <SYM> [--output <FILE>] [--duration <SECS>]
mercury replay   --file <FILE> [--speed <F64>]
mercury backtest --file <FILE> --strategy <NAME>
```

Config can also come from `mercury.toml` in the working directory:

```toml
symbol   = "BTCUSDT"
strategy = "market_maker"
paper    = false

[binance]
api_key    = "..."
secret_key = "..."

[risk]
max_position          = "1.0"
daily_loss_limit      = "500.0"
max_orders_per_second = 10

[metrics]
prometheus_port = 9090
```

## Architecture

Mercury is an event-driven trading engine. All inter-component communication flows through a single `EventBus` (`crates/core/src/event_bus.rs`) backed by a **custom lock-free MPMC ring buffer** (`crates/core/src/ring_buffer.rs`). The ring buffer is pre-allocated with 65,536 slots. Each `Subscriber` tracks its own read cursor independently — events are written once and fanned out with no per-subscriber copies. Slow subscribers are lossy: they skip forward and report `RecvError::Lagged`.

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
                                     IpcServer (/tmp/mercury.sock)   ← TUI
                                     ShmemServer (/tmp/mercury_shmem.bin) ← GUI
```

The engine exposes two IPC transports simultaneously: a Unix domain socket (`IpcServer`, `crates/core/src/ipc.rs`) used by the TUI, and a memory-mapped ring buffer (`ShmemServer`, `crates/core/src/shmem.rs`, ~4.2 MB, 1024 slots × 4 KB) used by the GUI for zero-copy event routing.

### Key traits

| Trait | Crate | Purpose |
| --- | --- | --- |
| `Strategy` | `mercury-strategy` | Implement for new strategies. Methods: `on_book`, `on_trade`, `on_fill`, `on_sentiment`, `reset`. |
| `ExchangeGateway` | `mercury-gateway` | Implement for new exchanges. Async: `submit_order`, `cancel_order`, `connect`. |
| `FeedParser` | `mercury-market` | Implement for new exchange WebSocket feeds. Methods: `parse`, `ws_url`, `subscribe_message`. |

### Crate dependency order

`core` ← `market`, `gateway`, `strategy`, `risk`, `execution`, `replay`, `metrics`, `storage` ← `cli`, `tui`, `gui`

- `core`: `Event`, `EventBus`, `RingBuffer`, `OrderBook`, `Pool`, `IpcServer`, `ShmemServer`/`ShmemClient`, all shared types
- `market`: `FeedManager`, `BinanceParser`, `CoinbaseParser`, `YahooFeed`, `PolymarketParser`, `KalshiParser`, `BookBuilder`
- `gateway`: `BinanceGateway`, `BybitGateway`, `CoinbaseGateway`, `KrakenGateway`, `OkxGateway`, `PolymarketGateway`, `KalshiGateway`, `PaperGateway`
- `strategy`: `StrategyRunner`, 10 built-in strategies, indicators (`Sma`, `Ema`, `Rsi`, `Macd`, `Atr`), ML components (see below)
- `risk`: `RiskManager` — position limits, daily loss guard, rate limiting, kill switch
- `execution`: `OrderManager`, `SimulatedExchange` (backtest), `SmartOrderRouter`, `ExecutionMetrics`, `TwapExecutor`, `VwapExecutor`, `PovExecutor`
- `replay`: `Recorder` (Parquet writer), `Player` (deterministic replay)
- `metrics`: `LatencyTracker` (HDR histograms), `PnlTracker`, Prometheus export
- `storage`: `StorageManager` — SQLite via sqlx; migrations in `crates/storage/migrations/`; use `:memory:` in tests
- `cli`: Binary `mercury` — `run`, `replay`, `backtest`, `record` subcommands
- `tui`: Binary `mercury-tui` — ratatui dashboard over `/tmp/mercury.sock`
- `gui`: Binary `mercury-gui` — Iced GPU-accelerated desktop console; reads from `/tmp/mercury_shmem.bin`
- `benches`: Criterion benchmarks; `allocations.rs` asserts **0 heap allocations** on the hot path

### EventPayload variants

`BookUpdate(Arc<BookUpdate>)`, `Trade`, `Signal`, `Order`, `Fill`, `RiskAlert`, `LatencyReport`, `MLPrediction`, `SentimentSignal`

### StrategyRunner and OrderBook

`StrategyRunner` maintains a `HashMap<Symbol, OrderBook>` and applies each `BookUpdate` before calling strategies. `on_book` always receives the fully-reconstructed L2 book, never the raw delta.

### Concurrency model

- Hot path is lock-free: `EventBus` uses atomic sequence numbers; no mutex on publish/receive.
- Long-running components are each spawned as `tokio::spawn` tasks.
- `StrategyRunner` calls strategies synchronously in a single task — `on_book`/`on_trade` take `&mut self` and must not block or allocate.

### Invariants to preserve

- **No `f64` for financial values.** Use `rust_decimal::Decimal` via the `FixedPoint` type alias and `dec!()` literals.
- **No heap allocation on the hot path.** The `allocations` benchmark enforces this.
- **`Symbol` is `[u8; 16]`** — stack-allocated, silently truncated. Use `Symbol::new("BTCUSDT")` or `"BTCUSDT".into()`.
- **`StrategyId` is `#[repr(u8)]`** with fixed discriminants — `Signal` is fully stack-allocated; do not change existing discriminant values.

### Execution algorithms

Large orders are worked via algorithmic executors in `crates/execution`. Each spawns a background task and emits child `Signal` events onto the bus.

| Executor | Logic |
| --- | --- |
| `TwapExecutor` | Equal slices at uniform time intervals |
| `VwapExecutor` | Slices sized proportionally to observed market volume |
| `PovExecutor` | Participates at a fixed fraction of market volume |

### ML components (`crates/strategy`)

| Component | File | Purpose |
| --- | --- | --- |
| `FeatureComputer` | `features.rs` | 12-feature vector (order imbalance, spread, RSI-14, MACD, depth ratio, EMA-50 deviation, ATR-14, VWAP deviation, depth slope, trade-flow imbalance, Kyle's λ, VPIN); `compute_extended()` adds Hawkes intensity as feature 13 |
| `RunningNormalizer` | `features.rs` | Welford online z-score per feature; warms up over 30 ticks |
| `OnlineClassifier` | `inference_strategy.rs` | Adam-SGD (β₁=0.9, β₂=0.999, lr=0.001, L2=1e-4); also supports ONNX via `tract-onnx` |
| `HmmFilter` | `regime.rs` | Two-state online HMM (Trending / MeanReverting); outputs `trend_probability` ∈ [0,1]; MarketMaker widens spread 2× when trending, tightens 0.75× when mean-reverting |
| `KyleLambda`, `RollSpread`, `Vpin` | `microstructure.rs` | Price-impact, effective half-spread, and trade-toxicity estimators |
| `checkpoint.rs` | `checkpoint.rs` | `clf.save/load("model.json")` for `OnlineClassifier`; `save/load_f32_weights` for DQN |

### Prediction markets

Polymarket and Kalshi use the same `OrderBook`, `BookUpdate`, and `Fill` types as crypto exchanges — prices are probabilities stored as `FixedPoint`. Because `Symbol` is 16 bytes, Mercury uses the last 16 characters of a Polymarket `condition_id` as the symbol key.

### Paper trading

`PaperGateway` (`crates/gateway/src/paper.rs`) matches orders against live `BookUpdate` events. Configurable RTT latency (default 5 ms). Use `--paper`; no API keys needed.

### Storage

SQLite (`mercury.db` by default). Only `Fill` events are persisted (to a `trades` table). Run migrations with sqlx at startup. Use `":memory:"` in tests.
