# Configuration

Mercury is configured primarily through CLI flags. There is no config file required for basic operation.

## CLI flags

| Flag | Default | Description |
|------|---------|-------------|
| `--symbol` | — | Trading symbol, e.g. `BTCUSDT` or `AAPL` |
| `--exchange` | `binance` | Exchange: `binance`, `coinbase`, `yahoo` |
| `--strategy` | — | Strategy name: `market_maker`, `rsi`, `momentum`, `arbitrage` |
| `--paper` | false | Enable paper trading via `PaperGateway` |
| `--log-level` | `info` | Tracing log level |

## Environment variables

| Variable | Description |
|----------|-------------|
| `BINANCE_API_KEY` | Binance REST/WS API key |
| `BINANCE_SECRET_KEY` | Binance HMAC secret |
| `COINBASE_API_KEY` | Coinbase Advanced Trade key |
| `COINBASE_SECRET_KEY` | Coinbase secret |

## Storage

Mercury writes its SQLite database to `mercury.db` in the current working directory. Override with `--db-path`:

```bash
cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy rsi --db-path /tmp/mercury.db
```

Use `:memory:` for ephemeral in-process storage (useful in tests).
