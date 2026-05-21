# Quick Start

## Paper trading (no API keys)

The fastest way to run Mercury is in paper-trading mode against a live Binance feed:

```bash
cargo run --bin mercury -- run \
  --symbol BTCUSDT \
  --paper \
  --strategy market_maker
```

Mercury will connect to the Binance WebSocket, receive live order-book updates, and simulate order matching with a 5 ms round-trip latency. No real orders are placed.

## Live trading

Set your exchange credentials as environment variables, then pass them at run time:

```bash
export BINANCE_API_KEY=your_key
export BINANCE_SECRET_KEY=your_secret

cargo run --bin mercury -- run \
  --symbol BTCUSDT \
  --strategy rsi
```

## TUI monitor

In a second terminal, start the dashboard:

```bash
cargo run --bin mercury-tui
```

The TUI connects to Mercury via a Unix socket at `/tmp/mercury.sock` and displays live order-book depth, fills, PnL, and latency percentiles.

## Adjust log verbosity

```bash
cargo run --bin mercury -- --log-level debug run --symbol BTCUSDT --paper --strategy rsi
```

Valid levels: `trace`, `debug`, `info`, `warn`, `error`.
