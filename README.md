# Mercury

## Low Latency Event Driven Trading & Execution Engine in Rust

[![Build Status](https://github.com/amjadjibon/mercury/workflows/CI/badge.svg)](https://github.com/amjadjibon/mercury/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Mercury is a **low latency event driven trading engine** designed for systematic and high frequency crypto trading.

It consumes real time order book feeds, generates trading signals, applies strict risk controls, executes orders across multiple venues, and provides deterministic replay for execution analysis and simulation.

The system is written in **production Rust** with a strong focus on:

- predictable latency
- lock free concurrency
- execution quality
- replay driven testing
- performance profiling

---

## Quick Start

```bash
# Build all crates
cargo build --workspace

# Run tests
cargo test --workspace

# Run CLI trading engine
cargo run --bin mercury -- run --symbol BTCUSDT --strategy market_maker

# Run with paper trading (testnet)
cargo run --bin mercury -- run --symbol BTCUSDT --paper

# Run TUI monitor
cargo run --bin mercury-tui

# Run benchmarks
cargo bench
```

---

## Goals

Mercury is designed to simulate real trading desk infrastructure rather than a retail trading bot.

Primary goals:

- process high frequency market data reliably
- make decisions under sub millisecond latency
- measure execution quality precisely
- enforce strict risk limits
- replay historical ticks deterministically
- benchmark and optimize hot paths

---

## Features

### Market Data

- real time Binance feeds (extensible via `FeedParser` trait)
- L2 order book reconstruction (snapshot + deltas)
- trade stream ingestion
- gap detection and automatic resync
- multi symbol support

### Strategy Engine

- pluggable `Strategy` trait interface
- event driven signal generation
- market making strategy with inventory skew
- momentum strategy based on trade flow

### Risk Controls

- max position limits
- max daily loss guard
- order rate limiting
- inventory skew control
- kill switch
- per venue exposure limits

### Execution

- limit and market orders
- fill tracking
- slippage and latency measurement
- order lifecycle management

### Replay & Simulation

- raw tick recording to parquet
- deterministic replay
- faster than realtime backtesting

### Observability

- p50/p99/p999 latency histograms (HDR)
- PnL tracking
- Prometheus metrics export
- structured tracing logs

### Performance

- lock free event bus (crossbeam)
- preallocated memory pools
- zero allocation hot path
- cache friendly data layout
- Criterion benchmark suite

---

## Architecture

```
                Market Feeds
         (WebSocket / FIX / REST snapshot)
                       │
                       ▼
               Market Data Layer
         (parser + order book rebuild)
                       │
                       ▼
               Lock Free Event Bus
                       │
        ┌──────────────┼──────────────┐
        ▼              ▼              ▼
   Strategy Engine   Risk Manager   Metrics
        │              │
        └──────────────┴───────► Execution
                                   │
                                   ▼
                           Exchange Gateways
                                   │
                                   ▼
                                Fills
                                   │
                                   ▼
                          Recorder / Replay
```

---

## Crate Structure

| Crate       | Description                                                       |
| ----------- | ----------------------------------------------------------------- |
| `core`      | Event bus, shared types, memory pool, ring buffer                 |
| `market`    | Feed ingestion, `FeedParser` trait, `BinanceParser`, book builder |
| `gateway`   | Exchange adapters, `ExchangeGateway` trait, Binance gateway       |
| `strategy`  | `Strategy` trait, market maker, momentum strategies               |
| `risk`      | `RiskManager`, position limits, daily loss guard, kill switch     |
| `execution` | `OrderManager`, fill tracking, slippage metrics                   |
| `replay`    | Tick recorder, deterministic player                               |
| `metrics`   | Latency tracker (HDR histogram), PnL tracker, Prometheus          |
| `cli`       | Command line interface (run, replay, backtest)                    |
| `tui`       | Terminal UI with ratatui (order book, stats)                      |
| `benches`   | Criterion benchmarks for event bus and order book                 |

```
mercury/
├── Cargo.toml              # Workspace configuration
├── crates/
│   ├── core/               # Event bus and shared types
│   ├── market/             # Feed ingestion and book rebuild
│   ├── gateway/            # Exchange connectors
│   ├── strategy/           # Trading logic
│   ├── risk/               # Guardrails and limits
│   ├── execution/          # Order lifecycle
│   ├── replay/             # Recording and playback
│   ├── metrics/            # Telemetry
│   ├── cli/                # Command line interface
│   ├── tui/                # Terminal UI
│   └── benches/            # Latency benchmarks
└── README.md
```

---

## CLI Usage

```bash
# Run trading engine
mercury run --symbol BTCUSDT --strategy market_maker

# Run with environment variables for API keys
BINANCE_API_KEY=xxx BINANCE_SECRET_KEY=yyy mercury run --symbol BTCUSDT

# Paper trading mode (uses testnet)
mercury run --symbol BTCUSDT --paper

# Replay historical data
mercury replay --file data.parquet --speed 1.0

# Backtest a strategy
mercury backtest --file data.parquet --strategy momentum
```

---

## Performance Characteristics

Designed targets:

- 100k+ messages/sec processing
- sub millisecond p99 decision latency
- zero allocations in hot path
- deterministic replay

Performance optimizations:

- lock free queues (crossbeam-channel)
- memory pooling (parking_lot)
- cache locality tuning
- Criterion benchmarks

---

## Example Use Cases

- market making
- cross exchange arbitrage
- execution quality research
- slippage analysis
- strategy parameter tuning
- latency experiments

---

## Why Rust

Rust was chosen for:

- memory safety without GC pauses
- predictable latency
- strong concurrency model
- zero cost abstractions
- production reliability

This makes it ideal for low latency trading systems.

---

## Dependencies

Key dependencies (latest versions):

| Dependency   | Version | Purpose              |
| ------------ | ------- | -------------------- |
| tokio        | 1.49    | Async runtime        |
| crossbeam    | 0.8     | Lock-free primitives |
| rayon        | 1.10    | CPU parallelism      |
| ratatui      | 0.30    | Terminal UI          |
| clap         | 4.5     | CLI parsing          |
| rust_decimal | 1.37    | Precise decimals     |
| parquet      | 54      | Tick storage         |
| hdrhistogram | 7.5     | Latency tracking     |

---

## License

MIT License - See [LICENSE](LICENSE) for details.

---

## Resume Summary

Example description:

> Built a low latency event driven trading engine in Rust consuming L2 order book data and executing across multiple exchanges. Implemented smart routing, strict risk controls, deterministic replay, and execution quality metrics. Optimized hot paths using lock free queues and memory preallocation to achieve sub millisecond p99 latency.

---
