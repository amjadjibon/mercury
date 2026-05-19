# Mercury — Roadmap

Build is clean, 98 tests pass. All original items complete.
This file tracks what comes next.

---

## Signal Generation

- [x] **Avellaneda-Stoikov market maker** — inventory-adjusted optimal quotes using closed-form solution. Replace `MarketMaker`'s fixed spread with `δ_bid = γσ²(T-t)/2 + (1/γ)ln(1 + γ/k)`. Needs `σ` (realised vol) and `γ` (risk aversion param). File: `crates/strategy/src/market_maker.rs`.

- [x] **Pairs statistical arbitrage** — OLS hedge ratio β = Cov(Y,X)/Var(X) on a rolling window. Entry when |z| > 2σ, exit when |z| < 0.5. `PairsStrategy` in `crates/strategy/src/pairs.rs`; `StrategyId::Pairs = 5` added to core.

- [x] **Order book imbalance (OBI) strategy** — `imbalance = (bid_qty − ask_qty) / (bid_qty + ask_qty)` summed over top-N levels. Buy when imbalance > threshold, sell when < −threshold; exit when |imb| < exit_threshold. `ObiStrategy` in `crates/strategy/src/obi.rs`; `OrderBook::imbalance(n)` in `crates/core/src/orderbook.rs`; `StrategyId::Obi = 6`. Note: not wired into `FeatureComputer` (would bump `FEATURE_COUNT` and break ONNX model input shape).

- [x] **Triangular arbitrage** — detect risk-free cycle across three pairs on one exchange. Forward: profit = bid_BTCUSDT × bid_ETHBTC / ask_ETHUSDT − 1. Reverse: profit = bid_ETHUSDT / (ask_BTCUSDT × ask_ETHBTC) − 1. Fires 3 market-order signals when either profit > `min_profit`. `TriangularStrategy` in `crates/strategy/src/triangular.rs`; `StrategyId::Triangular = 7`.

- [x] **Hawkes process trade arrival** — self-exciting point process; intensity λ(t) = μ + Σ α·exp(-β·(t-tᵢ)). Predicts short-term order flow bursts. `HawkesIntensity` lives in `crates/strategy/src/features.rs`; `FeatureComputer::compute_extended()` exposes it as feature 11 while keeping the existing ONNX 10-feature path stable.

- [x] **Regime detection (HMM)** — 2-state Hidden Markov Model (trending vs mean-reverting). `HmmFilter` in `crates/strategy/src/regime.rs` runs an online forward filter over return persistence and exposes regime probabilities for strategy parameter switching.

- [x] **Spoofing / iceberg detection** — spoofing: large quote appears then cancels before fill (track cancel-rate per price level). Iceberg: repeated fills at same price despite thin visible qty. Added to `crates/risk/src/manager.rs` as a signal quality filter.

- [x] **Volume prediction** — rolling ADV (average daily volume) estimator using exponential decay: `ADV_t = α·V_t + (1−α)·ADV_{t−1}`. `VolumeEstimator` added in `crates/strategy/src/features.rs`; kept separate from `FeatureComputer` to avoid changing ONNX input shape.

- [x] **Sentiment / news signal** — consume a REST or WebSocket news feed (e.g. Benzinga, CryptoPanic), run keyword scoring (+/− words), emit a `SentimentSignal` on the event bus within 50 ms of headline. `NewsParser` in `crates/market/src/news.rs` scores headlines, and `SentimentStrategy` in `crates/strategy/src/sentiment.rs` converts fresh high-confidence sentiment into signals.

---

## Execution Quality

- [x] **TWAP/VWAP slicer** — split a large signal into N child orders over T seconds, paced by volume. Add `TwapExecutor` in `crates/execution/src/twap.rs`. Takes a `Signal` + `duration_secs` + `slices` and emits child signals on a timer.

- [x] **Post-only / maker-rebate mode** — set post-only limit orders to earn maker rebate instead of paying taker fee. `TimeInForce::PostOnly` is wired through `Signal`, `OrderManager`, and gateway order payloads.

- [ ] **Peg orders** — re-quote at `best_bid ± tick` on every book update instead of cancel/replace. MarketMaker currently does cancel-replace; add a `peg_mode` flag that only re-quotes when the best price moves. File: `crates/strategy/src/market_maker.rs`.

- [x] **IOC / FOK time-in-force** — Immediate-Or-Cancel and Fill-Or-Kill needed for taker momentum strategies. TIF now flows from `Signal` to `Order`; Binance, OKX, Bybit, Coinbase, and Kraken map it in order payloads.

- [x] **Liquidity detection (probing)** — send a small resting limit order 1 tick inside the spread; if filled quickly, infer hidden liquidity and scale in. `LiquidityProber` in `crates/execution/src/probe.rs` tracks active probes, records quick probe fills separately, and `OrderManager` publishes scale-in signals.

---

## Risk & Sizing

- [x] **Kelly Criterion position sizing** — replace hardcoded `quantity=1.0` with `f* = edge / odds` computed from backtest win-rate and avg win/loss. Add `kelly_fraction()` to `crates/risk/src/manager.rs`. Cap at 0.25× full Kelly.

- [x] **Intraday trailing drawdown** — halt new orders when equity drops X% from session high (separate from daily loss limit). Track `session_high_pnl` in `RiskManager`, emit kill-switch when `(high - current) / high > threshold`. File: `crates/risk/src/manager.rs`.

- [x] **Correlation-aware joint position limit** — BTC and ETH are ~0.9 correlated; a long BTC + long ETH doubles directional exposure. `RiskManager::portfolio_delta(symbol)` sums positions weighted by a configurable correlation matrix, and pre-trade checks reject projected exposure above `max_portfolio_delta`. File: `crates/risk/src/manager.rs`.

- [x] **Inventory skew wired to quote pricing** — `MarketMaker` can use an external skew provider, and the CLI wires it to `RiskManager::skew(symbol)`. Long-heavy skew shifts both bid and ask down by `skew * σ` to lean toward selling. File: `crates/strategy/src/market_maker.rs`.

---

## Market Microstructure

- [x] **Realised volatility estimator** — 5-minute Parkinson estimator: `σ² = (ln(H/L))² / (4·ln2)`. Add `VolatilityEstimator` in `crates/strategy/src/volatility.rs`. Wired into MarketMaker — overrides EMA variance once warm (≥2 bars).

- [x] **Price impact model (Almgren-Chriss)** — `impact = η · σ · sqrt(Q / ADV)` where ADV is average daily volume. Added to `crates/risk/src/manager.rs` as a configurable pre-trade check using per-symbol ADV/volatility inputs.

- [x] **Queue position / fill probability** — estimate P(fill) at each price level from queue depth and historical fill rate. `FillProbabilityModel` in `crates/execution/src/queue_model.rs` converts eligible low-probability limit orders to IOC market orders when P(fill) < 0.4.

- [x] **Spread decomposition** — decompose observed spread into: adverse-selection component, inventory component, order-processing cost. `SpreadDecomposer` in `crates/metrics/` uses a Roll-style component plus imbalance pressure and exposes the components in the TUI stats panel.

---

## ML Pipeline

- [x] **Online learning** — update logistic regression weights incrementally with each new labeled tick (SGD). `OnlineClassifier` in `crates/strategy/src/inference_strategy.rs` provides a softmax logistic fallback when no ONNX model is loaded, using delayed book ticks for self-labeling.

- [ ] **Full LOB snapshot as CNN input** — replace 10-scalar feature vector with 20-level bid+ask volume profile fed into a 1D conv net. Requires generating a new ONNX model with input shape `[1, 2, 20]`. Update `FeatureComputer` in `crates/strategy/src/features.rs`.

- [ ] **Reinforcement learning quote placement** — Deep Q-Network agent where action = (spread_width, skew) and reward = PnL - inventory_penalty. Research-grade; needs a simulated environment wrapper around `SimulatedExchange`. Add `crates/strategy/src/rl_strategy.rs`.

---

## Infrastructure

- [ ] **`SO_BUSY_POLL` + isolated CPU core** — set `SO_BUSY_POLL=50` on the WebSocket socket and run the strategy thread on an isolated core (`isolcpus` kernel param). Linux only. File: `crates/market/src/feed.rs` + `crates/strategy/src/runner.rs`.

- [ ] **`SO_TIMESTAMPING` NIC timestamps** — parse `cmsg` ancillary data on the receive socket to get hardware NIC timestamp. Measures wire-to-strategy latency independent of `gettimeofday`. Linux only. File: `crates/market/src/feed.rs`.

- [ ] **`Arc<BookUpdate>` on event bus** — `BookUpdate` is 480 bytes; with 4 subscribers each copy costs ~1920 bytes per tick. Wrap in `Arc` so the bus stores one copy and subscribers share it. Change `EventPayload::BookUpdate(BookUpdate)` to `EventPayload::BookUpdate(Arc<BookUpdate>)`. File: `crates/core/src/events.rs`.

- [x] **WebSocket fill delivery** — Binance `listenKey` + user data stream implemented in `crates/gateway/src/binance.rs`. Parses `executionReport` events with `execType=TRADE`. Keepalive task runs every 30 min.

- [ ] **Feed reconnect tests** — reconnect logic in `crates/market/src/feed.rs` is untested. Add a test that simulates a dropped connection using a mock WebSocket server and asserts reconnect with backoff.

---

## Priority order

1. ~~Inventory skew → Avellaneda-Stoikov~~ ✅
2. ~~Kelly sizing + intraday trailing drawdown~~ ✅
3. ~~Realised volatility estimator~~ ✅
4. ~~WebSocket fill delivery~~ ✅
5. ~~TWAP slicer~~ ✅
6. ~~`i64` fixed-point~~ ✅
7. ~~Pairs stat-arb~~ ✅
8. ~~Order book imbalance~~ ✅
9. ~~Triangular arbitrage~~ ✅
10. ~~Volume prediction + Almgren-Chriss impact (dependency chain)~~ ✅
11. ~~Inventory skew wired to MarketMaker~~ ✅
12. ~~Correlation-aware position limits~~ ✅
13. ~~IOC/FOK + Post-only TIF~~ ✅
14. ~~Queue position / fill probability~~ ✅
15. ~~Spoofing / iceberg detection~~ ✅
16. ~~Spread decomposition (TUI metric)~~ ✅
17. ~~Hawkes process~~ ✅
18. ~~Regime detection (HMM)~~ ✅
19. ~~Liquidity probing~~ ✅
20. ~~Online learning (ML fallback)~~ ✅
21. ~~Sentiment / news signal~~ ✅
22. Full LOB CNN input
23. `Arc<BookUpdate>` bus optimization
24. Feed reconnect tests
25. `SO_BUSY_POLL` + NIC timestamps (Linux only)
26. RL quote placement (research-grade)

---

## HFT strategies coverage map (vs daytrading.com/hft-strategies)

| Strategy | Status |
|---|---|
| Market Making | ✅ Avellaneda-Stoikov |
| Statistical Arbitrage / Pair Trading | ✅ PairsStrategy |
| Cross-Market / Index Arbitrage | ✅ ArbitrageStrategy |
| TWAP / VWAP | ✅ TwapExecutor |
| Momentum | ✅ MomentumStrategy |
| Mean Reversion | ✅ RsiStrategy |
| ML Models | ✅ InferenceStrategy (ONNX) |
| Tick Data / FixedPoint hot path | ✅ FixedPoint i64 |
| Order Book Imbalance | ✅ ObiStrategy |
| Triangular Arbitrage | ✅ TriangularStrategy |
| Volume Prediction | ✅ VolumeEstimator |
| Price Impact (Almgren-Chriss) | ✅ pre-trade risk check |
| Inventory Skew | ✅ RiskManager skew wired to MarketMaker |
| Correlation Position Limits | ✅ correlation-weighted portfolio delta |
| IOC / FOK / Post-only | ✅ TIF carried through signals and gateways |
| Queue / Fill Probability | ✅ queue-depth model routes low-P(fill) limits to market |
| Iceberg / Spoofing Detection | ✅ risk signal-quality filter |
| Spread Decomposition | ✅ TUI metric via SpreadDecomposer |
| Hawkes Process (Order Flow) | ✅ HawkesIntensity extended feature |
| Regime Detection (HMM) | ✅ HmmFilter online forward filter |
| Liquidity Detection / Probing | ✅ LiquidityProber quick-fill scale-in |
| Online Learning (SGD) | ✅ OnlineClassifier fallback |
| Sentiment / News Signal | ✅ NewsParser + SentimentStrategy |
| Full LOB CNN | 📋 #22 |
| RL Quote Placement (DQN) | 📋 #26 |
| Quote Stuffing | ❌ unethical/illegal — not implementing |
| Regulatory Latency Arbitrage | ❌ out of scope |
