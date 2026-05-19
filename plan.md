# Mercury — Roadmap

Build is clean, 72 tests pass. All original items complete.
This file tracks what comes next.

---

## Signal Generation

- [x] **Avellaneda-Stoikov market maker** — inventory-adjusted optimal quotes using closed-form solution. Replace `MarketMaker`'s fixed spread with `δ_bid = γσ²(T-t)/2 + (1/γ)ln(1 + γ/k)`. Needs `σ` (realised vol) and `γ` (risk aversion param). File: `crates/strategy/src/market_maker.rs`.

- [x] **Pairs statistical arbitrage** — OLS hedge ratio β = Cov(Y,X)/Var(X) on a rolling window. Entry when |z| > 2σ, exit when |z| < 0.5. `PairsStrategy` in `crates/strategy/src/pairs.rs`; `StrategyId::Pairs = 5` added to core.

- [ ] **Hawkes process trade arrival** — self-exciting point process; intensity λ(t) = μ + Σ α·exp(-β·(t-tᵢ)). Predicts short-term order flow bursts. Add `HawkesIntensity` struct in `crates/strategy/src/features.rs`, feed as feature 10 into `FeatureComputer`.

- [ ] **Regime detection (HMM)** — 2-state Hidden Markov Model (trending vs mean-reverting). Switch strategy parameters based on detected regime. Add `HmmFilter` in `crates/strategy/src/regime.rs`.

- [ ] **Spoofing / iceberg detection** — spoofing: large quote appears then cancels before fill (track cancel-rate per price level). Iceberg: repeated fills at same price despite thin visible qty. Add to `crates/risk/src/manager.rs` as a signal quality filter.

---

## Execution Quality

- [x] **TWAP/VWAP slicer** — split a large signal into N child orders over T seconds, paced by volume. Add `TwapExecutor` in `crates/execution/src/twap.rs`. Takes a `Signal` + `duration_secs` + `slices` and emits child signals on a timer.

- [ ] **Post-only / maker-rebate mode** — set `post_only=true` on limit orders to earn maker rebate instead of paying taker fee. Add `time_in_force: TimeInForce::PostOnly` variant in `crates/core/src/types.rs` and wire into gateway order payloads.

- [ ] **Peg orders** — re-quote at `best_bid ± tick` on every book update instead of cancel/replace. MarketMaker currently does cancel-replace; add a `peg_mode` flag that only re-quotes when the best price moves. File: `crates/strategy/src/market_maker.rs`.

- [ ] **IOC / FOK time-in-force** — Immediate-Or-Cancel and Fill-Or-Kill needed for taker momentum strategies. Add variants to `TimeInForce` enum in `crates/core/src/types.rs`, wire through Binance/OKX/Bybit gateways.

---

## Risk & Sizing

- [x] **Kelly Criterion position sizing** — replace hardcoded `quantity=1.0` with `f* = edge / odds` computed from backtest win-rate and avg win/loss. Add `kelly_fraction()` to `crates/risk/src/manager.rs`. Cap at 0.25× full Kelly.

- [x] **Intraday trailing drawdown** — halt new orders when equity drops X% from session high (separate from daily loss limit). Track `session_high_pnl` in `RiskManager`, emit kill-switch when `(high - current) / high > threshold`. File: `crates/risk/src/manager.rs`.

- [ ] **Correlation-aware joint position limit** — BTC and ETH are ~0.9 correlated; a long BTC + long ETH doubles directional exposure. Add a `portfolio_delta()` method in `RiskManager` that sums positions weighted by correlation matrix. File: `crates/risk/src/manager.rs`.

- [ ] **Inventory skew wired to quote pricing** — `RiskManager::skew(symbol)` already exists but `MarketMaker` ignores it. Wire it: when long-heavy, shift both bid and ask down by `skew * σ` to lean toward selling. File: `crates/strategy/src/market_maker.rs`.

---

## Market Microstructure

- [x] **Realised volatility estimator** — 5-minute Parkinson estimator: `σ² = (ln(H/L))² / (4·ln2)`. Add `VolatilityEstimator` in `crates/strategy/src/volatility.rs`. Wired into MarketMaker — overrides EMA variance once warm (≥2 bars).

- [ ] **Price impact model (Almgren-Chriss)** — `impact = η · σ · sqrt(Q / ADV)` where ADV is average daily volume. Warn before submitting orders that would move the book. Add to `crates/risk/src/manager.rs` as a pre-trade check.

- [ ] **Queue position / fill probability** — estimate P(fill) at each price level from queue depth and historical fill rate. Use to choose limit vs market: if P(fill) < 0.4, send market order instead. Add `FillProbabilityModel` in `crates/execution/src/queue_model.rs`.

- [ ] **Spread decomposition** — decompose observed spread into: adverse-selection component, inventory component, order-processing cost. Measured via Roll model or Glosten-Harris. Expose as TUI metric. Add to `crates/metrics/`.

---

## ML Pipeline

- [ ] **Online learning** — update logistic regression weights incrementally with each new labeled tick (SGD). Avoids full retraining when market regime shifts. Add `OnlineClassifier` in `crates/strategy/src/inference_strategy.rs` as a fallback when no ONNX model is loaded.

- [ ] **Full LOB snapshot as CNN input** — replace 10-scalar feature vector with 20-level bid+ask volume profile fed into a 1D conv net. Requires generating a new ONNX model with input shape `[1, 2, 20]`. Update `FeatureComputer` in `crates/strategy/src/features.rs`.

- [ ] **Reinforcement learning quote placement** — Deep Q-Network agent where action = (spread_width, skew) and reward = PnL - inventory_penalty. Research-grade; needs a simulated environment wrapper around `SimulatedExchange`. Add `crates/strategy/src/rl_strategy.rs`.

---

## Infrastructure

- [ ] **`SO_BUSY_POLL` + isolated CPU core** — set `SO_BUSY_POLL=50` on the WebSocket socket and run the strategy thread on an isolated core (`isolcpus` kernel param). Linux only. File: `crates/market/src/feed.rs` + `crates/strategy/src/runner.rs`.

- [ ] **`SO_TIMESTAMPING` NIC timestamps** — parse `cmsg` ancillary data on the receive socket to get hardware NIC timestamp. Measures wire-to-strategy latency independent of `gettimeofday`. Linux only. File: `crates/market/src/feed.rs`.

- [ ] **`Arc<BookUpdate>` on event bus** — `BookUpdate` is 480 bytes; with 4 subscribers each copy costs ~1920 bytes per tick. Wrap in `Arc` so the bus stores one copy and subscribers share it. Change `EventPayload::BookUpdate(BookUpdate)` to `EventPayload::BookUpdate(Arc<BookUpdate>)`. File: `crates/core/src/events.rs`.

- [ ] **`i64` fixed-point prices** — `rust_decimal` division is ~30× slower than `f64`; `f64` has rounding errors. Use `i64` with implicit 8 decimal places (`price_i64 / 1e8`). Replace `Decimal` in hot-path structs (`Level`, `BookUpdate`, `Order`). Biggest latency win available.

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
