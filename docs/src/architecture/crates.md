# Crate Structure

Mercury is a Cargo workspace. Crates are ordered by dependency depth.

## Dependency order

```
core ← market, gateway, strategy, risk, execution, replay, metrics, storage ← cli, tui
```

## Crate descriptions

### `mercury-core`
Shared types and infrastructure used by every other crate.

- `Event`, `EventBus`, `EventPayload`
- `OrderBook`, `BookUpdate`, `Trade`
- `Pool` (object pool backed by `parking_lot`)
- `IpcServer` (Unix socket broadcaster)
- All domain types: `Symbol`, `Exchange`, `Side`, `Order`, `Fill`, `Signal`, `OrderId`, `TimeInForce`, `FixedPoint`

### `mercury-market`
Market data ingestion.

- `FeedManager` — manages WebSocket connections and reconnect logic
- `BinanceParser`, `CoinbaseParser` — implement `FeedParser`
- `YahooFeed` — HTTP polling feed for equities
- `BookBuilder` — reconstructs a full order book from incremental updates

### `mercury-gateway`
Exchange connectivity for order submission.

- `BinanceGateway`, `CoinbaseGateway`, `BybitGateway`, `KrakenGateway`, `OkxGateway`
- `PaperGateway` — local matching engine, configurable RTT latency
- All implement `ExchangeGateway`

### `mercury-strategy`
Strategy implementations and technical indicators.

- `StrategyRunner` — dispatches events to strategies
- Strategies: `MarketMaker`, `Momentum`, `RsiStrategy`, `ArbitrageStrategy`, `InferenceStrategy`
- Indicators: `SMA`, `EMA`, `RSI`, `MACD`

### `mercury-risk`
Pre-trade risk checks.

- `RiskManager` — position limits, daily loss guard, per-order rate limiting, kill switch

### `mercury-execution`
Order lifecycle management.

- `OrderManager` — wraps gateway with risk checks, tracks open orders
- `SimulatedExchange` — for backtesting
- `SmartOrderRouter` — best-execution routing across venues
- `ExecutionMetrics`

### `mercury-replay`
Data recording and deterministic replay.

- `Recorder` — writes tick data to Parquet files
- `Player` — replays a Parquet file at configurable speed

### `mercury-metrics`
Observability.

- `LatencyTracker` — HDR histograms for fill-to-ack, event-to-signal
- `PnlTracker` — realized and unrealized PnL
- Prometheus export endpoint

### `mercury-storage`
Persistence.

- `StorageManager` — SQLite via sqlx, persists `Fill` events to a `trades` table
- Migrations in `crates/storage/migrations/`

### `mercury-cli` (binary: `mercury`)
Top-level binary with subcommands: `run`, `replay`, `backtest`, `record`.

### `mercury-tui` (binary: `mercury-tui`)
Real-time ratatui dashboard. Connects to `mercury` via `/tmp/mercury.sock`.
