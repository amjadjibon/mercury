# Mercury Trading Engine

Mercury is a high-performance, event-driven algorithmic trading engine written in Rust. It is designed for low-latency market-making, momentum, and ML-based strategies across multiple exchanges.

## Key features

- **Event-driven core** — all components communicate through a single lock-free `EventBus` backed by a crossbeam bounded channel.
- **Multiple exchanges** — Binance, Coinbase, Bybit, Kraken, OKX, and Yahoo Finance (equities).
- **Paper trading** — built-in `PaperGateway` with configurable simulated latency; no API keys needed.
- **Backtesting** — deterministic replay of recorded Parquet tick data.
- **ML strategies** — ONNX-based inference, online logistic regression, and reinforcement learning.
- **Risk controls** — position limits, daily-loss guard, per-order rate limiting, and a kill switch.
- **TUI monitor** — real-time ratatui dashboard connected via Unix socket IPC.
- **Prometheus metrics** — latency histograms (HDR), PnL tracking, and Prometheus export.

## Design philosophy

Mercury favors explicitness and performance over abstraction. Financial values use `rust_decimal::Decimal` throughout — no floating-point arithmetic on prices or quantities. Hot-path data structures are stack-allocated where possible (e.g. `Symbol` is a `[u8; 16]` array).
