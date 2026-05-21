# Backtesting

The `backtest` subcommand replays a recorded Parquet file through a strategy and reports PnL.

## Run a backtest

```bash
cargo run --bin mercury -- backtest \
  --file data.parquet \
  --strategy market_maker
```

## How it works

1. `Player` reads the Parquet file and replays `BookUpdate` events in timestamp order.
2. `StrategyRunner` drives the strategy as if events were live.
3. `SimulatedExchange` matches orders against the replayed book with zero latency (conservative: assumes worst-case fill at the top of the book).
4. Results are printed to stdout and written to `backtest_results.json`.

## Output

```
Symbol:   BTCUSDT
Strategy: market_maker
Trades:   1,243
Win rate: 54.2%
PnL:      +$312.47
Max DD:   -$48.20
Sharpe:   1.84
```

## Tips

- Record a representative data set (at least a few hours) before running a backtest.
- Use `--speed 0` to replay as fast as possible (no wall-clock sleep between ticks).
- Run multiple strategies on the same file to compare them fairly.
