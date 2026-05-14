# Mercury

## Low-Latency Event-Driven Trading Engine in Rust

[![Build Status](https://github.com/amjadjibon/mercury/workflows/CI/badge.svg)](https://github.com/amjadjibon/mercury/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Mercury is a low-latency, event-driven trading engine for systematic and high-frequency crypto trading. It consumes real-time order book feeds, generates signals, enforces risk limits, executes orders across multiple venues, and supports deterministic replay for backtesting and analysis.

---

## Quick Start

```bash
# Build
cargo build --workspace

# Test
cargo test --workspace

# Paper trade (no API keys needed — matches against live Binance data)
cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy market_maker

# Live trade (Binance)
BINANCE_API_KEY=xxx BINANCE_SECRET_KEY=yyy \
  cargo run --bin mercury -- run --symbol BTCUSDT --strategy market_maker

# Record 60s of live ticks to Parquet
cargo run --bin mercury -- record --symbol BTCUSDT --output data.parquet

# Backtest against recorded data
cargo run --bin mercury -- backtest --file data.parquet --strategy market_maker

# Replay recorded data (1× real-time or max speed)
cargo run --bin mercury -- replay --file data.parquet --speed 1.0

# TUI monitor (connect to a running engine)
cargo run --bin mercury-tui

# Benchmarks
cargo bench
```

---

## CLI Reference

```
mercury run      --symbol <SYM> [--exchange binance|coinbase|yahoo]
                 [--strategy market_maker|momentum|rsi|arbitrage|inference]
                 [--paper]
                 [--api-key <KEY>] [--secret-key <SECRET>]
                 [--log-level trace|debug|info|warn|error]

mercury record   --symbol <SYM> [--output <FILE>] [--duration <SECS>]
mercury replay   --file <FILE> [--speed <F64>]
mercury backtest --file <FILE> --strategy <NAME>
```

Config can also be supplied via `mercury.toml` in the working directory:

```toml
symbol   = "BTCUSDT"
strategy = "market_maker"
paper    = false

[binance]
api_key    = "..."
secret_key = "..."

[risk]
max_position       = "1.0"
daily_loss_limit   = "500.0"
max_orders_per_second = 10

[metrics]
prometheus_port = 9090
```

---

## Architecture

```
Market Feeds (WebSocket)
        │
        ▼
   FeedManager          ← BinanceParser / CoinbaseParser / YahooFeed
        │
        ▼
   EventBus             ← LMAX Disruptor-style ring buffer, lock-free fan-out
        │
   ┌────┴─────────────────────────────┐
   ▼                                  ▼
StrategyRunner (dedicated OS thread)  OrderManager
  on_book / on_trade / on_fill          ↓ risk checks
        │                           ExchangeGateway
        ▼                             (Binance / Coinbase / Paper)
   Signal events                         │
   → EventBus                        Fill events → EventBus
                                          │
                                     StorageManager (SQLite)
                                     IpcServer (/tmp/mercury.sock)
                                     → TUI
```

**Hot path**: feed parser → ring buffer publish → strategy spin-loop → signal → order submit. No heap allocation, no locks.

---

## Crate Structure

| Crate | Description |
|---|---|
| `core` | `Event`, `EventBus` (ring buffer), shared types (`Symbol`, `Side`, `Order`, `Fill`, `Signal`), `IpcServer`, `Pool` |
| `market` | `FeedManager`, `BinanceParser`, `CoinbaseParser`, `YahooFeed`, `BookBuilder` |
| `gateway` | `BinanceGateway`, `CoinbaseGateway`, `PaperGateway`, `ExchangeGateway` trait |
| `strategy` | `StrategyRunner`, `MarketMaker`, `Momentum`, `RsiStrategy`, `ArbitrageStrategy`, `InferenceStrategy`; indicators: SMA, EMA, RSI, MACD |
| `risk` | `RiskManager` — position limits, daily loss guard, order rate limiter, kill switch |
| `execution` | `OrderManager`, `SimulatedExchange` (backtest), `SmartOrderRouter`, `ExecutionMetrics` |
| `replay` | `Recorder` (Parquet writer), `Player` (deterministic replay) |
| `metrics` | `LatencyTracker` (HDR histograms), `PnlTracker`, Prometheus export |
| `storage` | `StorageManager` — SQLite via sqlx, persists fills |
| `cli` | Binary `mercury` — `run`, `record`, `replay`, `backtest` subcommands |
| `tui` | Binary `mercury-tui` — ratatui dashboard over IPC |
| `benches` | Criterion benchmarks; `allocations.rs` asserts 0 heap allocs on hot path |

---

## Strategies

| Name | Trigger | Logic |
|---|---|---|
| `market_maker` | `on_book` | Posts bid/ask around mid; cancel-replaces on each update; inventory skew |
| `momentum` | `on_trade` | Tracks buy/sell volume ratio over a window; signals when imbalance exceeds threshold |
| `rsi` | `on_trade` | RSI < 30 → buy; RSI > 70 → sell |
| `arbitrage` | `on_book` | Monitors BBO across exchanges; signals when Bid(A) > Ask(B) + min_profit |
| `inference` | — | Stub; wired but emits no signals until ML backend is added |

Implement the `Strategy` trait to add a new strategy:

```rust
pub trait Strategy: Send {
    fn id(&self) -> StrategyId;
    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal>;
    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal>;
    fn on_fill(&mut self, fill: &Fill);
    fn reset(&mut self);
}
```

---

## Performance

Measured on an M-series Mac; Linux co-location numbers will be lower.

| Hot-path metric | Baseline | After optimisation | Target |
|---|---|---|---|
| `orderbook/apply_10_levels` | 625 ns | 485 ns | < 200 ns |
| `strategy/process_book_update` | 239 ns | 222 ns | < 100 ns |
| `strategy/market_maker_on_book` | 45 ns | 37 ns | < 20 ns |
| `event_bus/roundtrip` | 290 ns | 210 ns | < 100 ns |
| Hot-path heap allocations | many | **0** | 0 |

Key techniques applied:

- **Ring buffer EventBus** — LMAX Disruptor pattern; publisher writes once, all subscribers read the same pre-allocated slots with independent cursors (`UnsafeCell` + `AtomicU64`)
- **Fixed-size arrays** — `BookUpdate` carries `[Level; 20]` instead of `Vec<Level>`; `StrategyId` is `#[repr(u8)]` instead of `String`
- **Dedicated strategy thread** — `StrategyRunner::run_on_thread(core_id)` spins on the ring buffer off the tokio thread pool; pinned to a specific core via `core_affinity`
- **TCP_NODELAY** on WebSocket connections; `SO_BUSY_POLL` on Linux
- `stats_alloc` allocation tests enforce the zero-alloc invariant in CI

---

## Observability

While the engine is running:

```bash
# Prometheus metrics
curl http://localhost:9090/metrics

# Structured logs (default INFO; override with --log-level)
cargo run --bin mercury -- --log-level debug run --symbol BTCUSDT --paper
```

Exposed metrics: `mercury_fills_total`, `mercury_orders_submitted_total`, `mercury_strategy_latency_p50_ns`, `mercury_strategy_latency_p99_ns`, `mercury_strategy_latency_p999_ns`.

---

## Risk Controls

Configured via `mercury.toml` or `RiskConfig` defaults:

- max position size per symbol
- daily loss limit (kills new orders when breached)
- max orders per second (rate limiter)
- kill switch (halts all order submission)

---

## Dependencies

| Crate | Version | Purpose |
|---|---|---|
| `tokio` | 1.49 | Async runtime |
| `crossbeam-channel` | 0.5 | Feed/recorder channels |
| `parking_lot` | 0.12 | Memory pool mutex |
| `core_affinity` | 0.8 | CPU thread pinning |
| `ratatui` | 0.30 | Terminal UI |
| `clap` | 4.5 | CLI parsing |
| `rust_decimal` | 1.37 | Precise financial arithmetic |
| `parquet` / `arrow` | 54 | Tick storage |
| `hdrhistogram` | 7.5 | Latency percentiles |
| `sqlx` | 0.7 | SQLite fill persistence |
| `metrics` + `metrics-exporter-prometheus` | 0.24 / 0.16 | Prometheus export |

---

## License

MIT — see [LICENSE](LICENSE).
