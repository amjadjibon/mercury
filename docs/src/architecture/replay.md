# Replay & Recorder

The `mercury-replay` crate provides tools for recording live market data and replaying it deterministically.

## Recorder

`Recorder` subscribes to `BookUpdate` and `Trade` events on the bus and writes them to a Parquet file.

```bash
# Record 120 seconds of BTCUSDT tick data
cargo run --bin mercury -- record \
  --symbol BTCUSDT \
  --output data.parquet \
  --duration 120
```

Default duration is 60 seconds. The output file can be replayed or used for backtesting.

## Player

`Player` reads a Parquet file and replays events onto the `EventBus` at a configurable speed multiplier.

```bash
# Replay at real-time speed
cargo run --bin mercury -- replay --file data.parquet --speed 1.0

# Replay at 10x speed
cargo run --bin mercury -- replay --file data.parquet --speed 10.0
```

## Backtest

The `backtest` subcommand combines `Player` with a `SimulatedExchange` to run a strategy over historical data and report PnL:

```bash
cargo run --bin mercury -- backtest \
  --file data.parquet \
  --strategy market_maker
```

Results are printed to stdout and written to `backtest_results.json`.

## Parquet schema

Each row represents one tick:

| Column | Type | Description |
|--------|------|-------------|
| `timestamp` | `INT64` | Unix millis |
| `symbol` | `BYTE_ARRAY` | Trading symbol |
| `bid_price` | `BYTE_ARRAY` | Best bid (decimal string) |
| `ask_price` | `BYTE_ARRAY` | Best ask (decimal string) |
| `bid_qty` | `BYTE_ARRAY` | Best bid quantity |
| `ask_qty` | `BYTE_ARRAY` | Best ask quantity |
