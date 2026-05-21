# Recording Market Data

The `record` subcommand captures live tick data to a Parquet file for later replay or backtesting.

## Basic usage

```bash
# Record 60 seconds (default)
cargo run --bin mercury -- record --symbol BTCUSDT --output data.parquet

# Record 10 minutes
cargo run --bin mercury -- record --symbol BTCUSDT --output data.parquet --duration 600
```

## Output format

The Parquet file contains one row per `BookUpdate` event with columns:

- `timestamp` — Unix milliseconds
- `symbol` — trading symbol
- `bid_price`, `bid_qty` — best bid
- `ask_price`, `ask_qty` — best ask

Prices and quantities are stored as UTF-8 decimal strings to preserve exact precision.

## Storage size

Roughly 1–3 MB per minute for a single symbol at normal Binance update frequency (~10 updates/sec). A one-hour recording of BTCUSDT is typically 60–180 MB.

## Using recorded data

```bash
# Replay at real-time speed
cargo run --bin mercury -- replay --file data.parquet --speed 1.0

# Run a backtest
cargo run --bin mercury -- backtest --file data.parquet --strategy rsi
```
