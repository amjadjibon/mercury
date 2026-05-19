# Mercury — What's Left To Do

Current state: builds clean, 72 tests pass. All items complete.

Legend: ✅ done · 🔲 remaining

---

## ✅ Done (all sessions)

| Item | What was done |
|---|---|
| 1a | PnL history chart in TUI (price top, PnL bottom, color-coded) |
| 1b | TUI `--symbol` / `--exchange` CLI args |
| 1c | DepthWidget replaced with real cumulative bid/ask depth curves |
| 2a | `orders` table written on every `Signal` event |
| 2b | `latency_snapshots` table + migration, written every 10 reports |
| 3  | Crash recovery: `open_orders()` on all gateways, `restore_open_orders()` in OrderManager, wired in CLI |
| 4  | Coinbase gateway: real HMAC-SHA256 signing, full REST order flow |
| 5a | ArbitrageStrategy pending-order guard (no more signal spam) |
| 5b | RsiStrategy position tracked in `on_fill` only |
| 5c | MarketMaker cancel_replace fixed on ask side |
| 5d | Momentum exit logic (closes on momentum reversal) |
| 5e | InferenceStrategy: ONNX via tract, 10-feature vector, MLPrediction events |
| 6  | Backtest: Sharpe, win rate, profit factor, avg win/loss, trade log |
| 7a | `RiskManager::skew(symbol)` wired to `calculate_skew` |
| 7b | Daily PnL reset task at UTC midnight |
| 7c | Fill toxicity detection + kill switch integration |
| 8  | WebSocket reconnect with exponential backoff (100ms→60s) |
| 9  | OrderBook: sorted Vec with binary search (was BTreeMap), cached mid_price |
| 10 | Multi-symbol: `--symbol` accepts Vec<String>; TUI tab bar (←/→ to switch) |
| 11 | OKX / Bybit / Kraken feed parsers + gateways + CLI routing |
| 12 | Full test coverage: SOR (2), ArbitrageStrategy (3), ExecutionMetrics (4), InferenceStrategy (4), e2e pipeline (1) |
| ML | Dataset CLI subcommand, FeatureComputer (10 features), feature_snapshots SQLite table |

---

## Possible future work

- `SO_TIMESTAMPING` — wire-to-app NIC latency (Linux only, requires kernel socket options)
- `Arc<BookUpdate>` on event bus — reduce 480-byte copies to pointer copies for high-subscriber scenarios
- `SO_BUSY_POLL` + CPU affinity enforcement for true HFT latency floor
- Replace `rust_decimal` with `i64` fixed-point on the hot path (~10–50× faster arithmetic)
- Production WebSocket fill delivery (Binance user data stream, Coinbase user channel)
- Feed reconnect unit tests (`crates/market/src/feed.rs` reconnect logic)
