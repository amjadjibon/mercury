# Mercury

## Low-Latency Event-Driven Trading Engine in Rust

[![Build Status](https://github.com/amjadjibon/mercury/workflows/CI/badge.svg)](https://github.com/amjadjibon/mercury/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Docs](https://img.shields.io/badge/docs-github--pages-blue)](https://amjadjibon.github.io/mercury/)

Mercury is a low-latency, event-driven trading engine for systematic and high-frequency trading across crypto exchanges and prediction markets. It consumes real-time order book feeds, generates signals, enforces risk limits, executes orders across multiple venues, and supports deterministic replay for backtesting and analysis.

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

Config can also be supplied via `mercury.toml` in the working directory:

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

---

## Architecture

```
Market Feeds (WebSocket)
        │
        ▼
   FeedManager          ← BinanceParser / CoinbaseParser / YahooFeed
        │
        ▼
   EventBus             ← lock-free MPMC ring buffer, independent subscriber cursors
        │
   ┌────┴─────────────────────────────┐
   ▼                                  ▼
StrategyRunner (dedicated OS thread)  OrderManager
  on_book / on_trade / on_fill          ↓ risk checks
        │                           ExchangeGateway
        ▼         (Binance/Coinbase/Bybit/Kraken/OKX/Polymarket/Kalshi/Paper)
   Signal events                         │
   → EventBus                        Fill events → EventBus
                                          │
                                     StorageManager (SQLite)
                                     IpcServer (/tmp/mercury.sock)
                                     → TUI
```

**Hot path**: feed parser → ring buffer publish → strategy spin-loop → signal → order submit. No heap allocation, no locks.

The `EventBus` is a custom MPMC ring buffer (65,536 pre-allocated slots). Each subscriber tracks its own read cursor — events are written once and fanned out with no copies. Slow subscribers are lossy: they skip forward and report `Lagged`. `StrategyRunner` maintains a `HashMap<Symbol, OrderBook>` and reconstructs the full L2 book before calling strategies, so `on_book` always receives consistent state.

---

## Crate Structure

| Crate | Description |
|---|---|
| `core` | `Event`, `EventBus` (ring buffer), shared types (`Symbol`, `Side`, `Order`, `Fill`, `Signal`, `StrategyId`), `IpcServer`, `Pool` |
| `market` | `FeedManager`, `BinanceParser`, `CoinbaseParser`, `YahooFeed`, `PolymarketParser`, `KalshiParser`, `BookBuilder` |
| `gateway` | `BinanceGateway`, `CoinbaseGateway`, `BybitGateway`, `KrakenGateway`, `OkxGateway`, `PolymarketGateway`, `KalshiGateway`, `PaperGateway`, `ExchangeGateway` trait |
| `strategy` | `StrategyRunner`, `MarketMaker`, `Momentum`, `RsiStrategy`, `ArbitrageStrategy`, `InferenceStrategy`, `RlQuotePlacementStrategy`, `PairsStrategy`, `ObiStrategy`, `TriangularStrategy`, `SentimentStrategy`; indicators: SMA, EMA, RSI, MACD, ATR |
| `risk` | `RiskManager` — position limits, daily loss guard, order rate limiter, kill switch |
| `execution` | `OrderManager`, `SimulatedExchange` (backtest), `SmartOrderRouter`, `ExecutionMetrics`, `TwapExecutor`, `VwapExecutor`, `PovExecutor` |
| `replay` | `Recorder` (Parquet writer), `Player` (deterministic replay) |
| `metrics` | `LatencyTracker` (HDR histograms), `PnlTracker`, Prometheus export |
| `storage` | `StorageManager` — SQLite via sqlx, persists fills |
| `cli` | Binary `mercury` — `run`, `record`, `replay`, `backtest` subcommands |
| `tui` | Binary `mercury-tui` — ratatui dashboard over IPC |
| `benches` | Criterion benchmarks; `allocations.rs` asserts 0 heap allocs on hot path |

---

## Strategies

| Name | Flag | Trigger | Logic |
|---|---|---|---|
| Market Maker | `market_maker` | `on_book` | Symmetric quotes with inventory skew and HMM regime gating; widens spread 2× when trending, tightens 0.75× when mean-reverting |
| Momentum | `momentum` | `on_trade` | EMA crossover; signals on fast/slow cross |
| RSI | `rsi` | `on_trade` | RSI < 30 → buy; RSI > 70 → sell |
| Arbitrage | `arbitrage` | `on_book` | Cross-exchange BBO spread; signals when Bid(A) > Ask(B) + min_profit |
| Inference | `inference` | `on_book` | ONNX or Adam-SGD online classifier on 12-feature vector (10 market + Kyle's λ + VPIN) with Welford z-score normalisation |
| RL Quote Placement | `rl` | `on_book` | DQN quote placement with 7-state input (6 market features + HMM trend probability) and experience replay |
| Pairs | `pairs` | `on_book` | Statistical arbitrage on correlated symbol pairs |
| OBI | `obi` | `on_book` | Order book imbalance signals |
| Triangular | `triangular` | `on_book` | Three-leg crypto triangular arbitrage |
| Sentiment | `sentiment` | `on_sentiment` | News keyword scoring via `SentimentSignal` events |

Implement the `Strategy` trait to add a new strategy:

```rust
pub trait Strategy: Send + Sync {
    fn id(&self) -> StrategyId;
    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal>;
    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal>;
    fn on_fill(&mut self, fill: &Fill);
    fn on_sentiment(&mut self, signal: &SentimentSignal) -> Vec<Signal> { vec![] }
    fn reset(&mut self);
}
```

---

## Execution Algorithms

Large parent orders can be worked via algorithmic executors in `crates/execution`. Each executor spawns a background task and emits child `Signal` events onto the `EventBus` for `OrderManager` to pick up.

| Executor | Type | Logic |
|---|---|---|
| `TwapExecutor` | Time-weighted average price | Divides parent quantity into `N` equal slices emitted at uniform time intervals |
| `VwapExecutor` | Volume-weighted average price | Sizes each slice proportionally to observed market trade volume; falls back to equal-weight when no volume is seen |
| `PovExecutor` | Percentage of volume | Participates at a fixed fraction of observed market volume, bounded by min/max slice size; flushes any residual at deadline |

```rust
// TWAP — 10 equal slices over 5 minutes
TwapExecutor::new(bus.clone()).execute(signal, 300, 10);

// VWAP — 8 volume-proportional slices over 5 minutes
VwapExecutor::new(bus.clone()).execute(signal, 300, 8);

// POV — participate at 10% of market volume, slices between 0.01 and 1.0, 10-minute deadline
PovExecutor::new(bus.clone()).execute(
    signal,
    0.10,                              // participation_rate
    FixedPoint::from_decimal(dec!(0.01)), // min_slice_qty
    FixedPoint::from_decimal(dec!(1.0)),  // max_slice_qty
    600,                               // deadline_secs
);
```

---

## ML Components

Mercury's `mercury-strategy` crate ships production-ready machine-learning building blocks that strategies compose at runtime — no Python, no external service.

### Feature vector (12 features, `FeatureComputer`)

| # | Feature | Notes |
|---|---------|-------|
| 0 | Order imbalance | `(bid_qty − ask_qty) / (bid_qty + ask_qty)` ∈ [−1, 1] |
| 1 | Spread bps / 100 | Normalised bid-ask spread |
| 2 | RSI-14 / 100 | |
| 3 | MACD histogram sign × magnitude | Clamped to [−1, 1] |
| 4 | Depth ratio (top-5 bid / total) | |
| 5 | EMA-50 deviation | `(mid − ema50) / ema50` |
| 6 | ATR-14 / mid | Normalised average true range |
| 7 | VWAP deviation (100-trade rolling) | |
| 8 | Depth slope | `(bid[0] − bid[4]) / mid` |
| 9 | Trade-flow imbalance (20-trade rolling) | Buy volume fraction ∈ [0, 1] |
| 10 | **Kyle's Lambda** | `tanh(λ × 1000)` — price-impact coefficient from rolling OLS on signed order flow ∈ (−1, 1) |
| 11 | **VPIN** | Volume-synchronised probability of informed trading ∈ [0, 1]; >0.5 signals toxic flow |

`compute_extended()` appends a 13th feature: Hawkes trade-arrival intensity.

### Online normalisation (`RunningNormalizer`)

Welford one-pass algorithm maintains running mean and variance per feature. After a 30-tick warm-up, `InferenceStrategy` standardises the 12-feature vector to approximately zero mean / unit variance before feeding the Adam-SGD classifier. Converges 3–5× faster than unnormalised features on volatile symbols.

### Adam optimizer (`OnlineClassifier`)

Replaces vanilla SGD. Uses bias-corrected first/second moment estimates (β₁ = 0.9, β₂ = 0.999, ε = 1e-8, lr = 0.001). L2 regularisation (`l2 = 1e-4`) prevents weight explosion on stationary features.

### HMM regime filter (`HmmFilter`)

Two-state online forward filter (`Trending` / `MeanReverting`) driven by consecutive log-return sign persistence. Outputs a `trend_probability` ∈ [0, 1] after two price updates; confident regime declared above 0.60 threshold.

**MarketMaker integration** — spread multiplier applied on every `on_book` call:

| Regime | Spread multiplier |
|--------|-------------------|
| Trending | 2.0× (widen — adverse selection risk) |
| MeanReverting | 0.75× (tighten — capture more mean-reversion fills) |
| Uncertain | 1.0× (no change) |

**DQN integration** — `RlQuotePlacementStrategy` appends `trend_probability − 0.5` as state dimension 6 (0 = uncertain, +0.5 = fully trending, −0.5 = fully mean-reverting). The network architecture expands from 6 → 7 → 24 → 12 → 9.

### Microstructure estimators (`microstructure.rs`)

| Struct | Algorithm | Output |
|--------|-----------|--------|
| `KyleLambda` | Rolling OLS: `λ = Σ(Q·ΔP) / Σ(Q²)` | Price-impact coefficient; rising λ signals informed flow |
| `RollSpread` | Roll (1984): `c = √(−Cov(ΔP_t, ΔP_{t−1}))` | Effective half-spread estimate |
| `Vpin` | Bucket-based `\|buy_vol − sell_vol\| / bucket_size` | Trade toxicity ∈ [0, 1] |

### Model checkpointing (`checkpoint.rs`)

```rust
// Save/load OnlineClassifier weights (compact JSON).
clf.save("model.json")?;
clf.load("model.json")?;

// Save/load DQN network weights (raw f32 LE binary).
save_f32_weights("dqn.bin", &[&w1_flat, &w2_flat, &w3_flat])?;
let weights = load_f32_weights("dqn.bin", total_params)?;
```

---

## Prediction Markets

Mercury supports [Polymarket](https://polymarket.com) and [Kalshi](https://kalshi.com) alongside crypto exchanges. Binary outcome contracts (YES/NO shares priced 0–100 cents) map directly onto the existing `OrderBook`, `BookUpdate`, and `Fill` types — prices are stored as probabilities using `FixedPoint`.

### Polymarket

Markets are identified by a 64-character hex `condition_id`. Because `Symbol` is 16 bytes, Mercury uses the last 16 characters of the condition ID as the symbol key (the low-order bytes are unique per active market).

```bash
# Paper trade against a live Polymarket CLOB feed (no API keys needed)
cargo run --bin mercury -- run \
  --exchange polymarket \
  --symbol 0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af \
  --paper \
  --strategy momentum

# Live trading
EXCHANGE_API_KEY=your_api_key \
EXCHANGE_SECRET_KEY=your_api_secret \
POLYMARKET_PASSPHRASE=your_passphrase \
  cargo run --bin mercury -- run \
  --exchange polymarket \
  --symbol 0x...condition_id... \
  --strategy rsi
```

### Kalshi

Market tickers (e.g. `TRUMPWIN-2024`) fit directly in the 16-byte `Symbol`. Authentication uses an RSA private key; pass either the PEM string or a path to a PEM file.

```bash
# Live trading
KALSHI_API_KEY_ID=your-uuid-key-id \
KALSHI_PRIVATE_KEY=./kalshi_key.pem \
  cargo run --bin mercury -- run \
  --exchange kalshi \
  --symbol TRUMPWIN-2024 \
  --strategy rsi
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

Key techniques:

- **Ring buffer EventBus** — publisher writes once; all subscribers read the same pre-allocated slots with independent `AtomicU64` cursors
- **Fixed-size arrays** — `BookUpdate` carries `[Level; 20]` instead of `Vec<Level>`; `StrategyId` is `#[repr(u8)]` so `Signal` is fully stack-allocated
- **Dedicated strategy thread** — `StrategyRunner` can be pinned to a specific CPU core via `core_affinity`
- **TCP_NODELAY** on WebSocket connections; `SO_BUSY_POLL` on Linux
- `stats_alloc` allocation tests enforce the zero-alloc invariant in CI

---

## Observability

```bash
# Prometheus metrics
curl http://localhost:9090/metrics

# Structured logs
cargo run --bin mercury -- --log-level debug run --symbol BTCUSDT --paper
```

Exposed metrics: `mercury_fills_total`, `mercury_orders_submitted_total`, `mercury_strategy_latency_p50_ns`, `mercury_strategy_latency_p99_ns`, `mercury_strategy_latency_p999_ns`.

---

## Risk Controls

Configured via `mercury.toml` or `RiskConfig` defaults:

- Max position size per symbol
- Daily loss limit (kills new orders when breached)
- Max orders per second (token-bucket rate limiter)
- Kill switch — halts all order submission and cancels open orders

---

## Documentation

Full architecture docs and guides are available at **[amjadjibon.github.io/mercury](https://amjadjibon.github.io/mercury/)** or build locally:

```bash
cargo install mdbook
mdbook serve docs --open
```

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
| `tract-onnx` | 0.21 | Pure-Rust ONNX inference (no system library) |
| `rsa` | 0.9 | RSA-SHA256 signing for Kalshi auth |
| `metrics` + `metrics-exporter-prometheus` | 0.24 / 0.16 | Prometheus export |

---

## License

MIT — see [LICENSE](LICENSE).
