# Mercury — What's Left To Do

Current state: builds clean, 58 tests pass. All quick wins and items 1–8, 11, 12 (partial) done.

Legend: ✅ done · 🔲 remaining

---

## ✅ Done (last session)

| Item | What was done |
|---|---|
| 1a | PnL history chart in TUI (price top, PnL bottom, color-coded) |
| 1b | TUI `--symbol` / `--exchange` CLI args |
| 1c | DepthWidget replaced with real cumulative bid/ask depth curves |
| 2a | `orders` table written on every `Signal` event |
| 2b | `latency_snapshots` table + migration, written every 10 reports |
| 5a | ArbitrageStrategy pending-order guard (no more signal spam) |
| 5b | RsiStrategy position tracked in `on_fill` only |
| 5c | MarketMaker cancel_replace fixed on ask side |
| 5d | Momentum exit logic (closes on momentum reversal) |
| 6  | Backtest: Sharpe, win rate, profit factor, avg win/loss, trade log |
| 7a | `RiskManager::skew(symbol)` wired to `calculate_skew` |
| 7b | Daily PnL reset task at UTC midnight |
| 7c | Fill toxicity detection + kill switch integration |
| 8  | WebSocket reconnect with exponential backoff (100ms→60s) |
| 11 | OKX / Bybit / Kraken feed parsers + gateways + CLI routing |
| 12 | SOR router tests (2), ArbitrageStrategy tests (3) |

---

## 🔲 3. Crash Recovery

On restart open orders are not reloaded. A crash mid-trade leaves an unknown position.

**Files:** `crates/gateway/src/binance.rs`, `crates/execution/src/order_manager.rs`

**Work:**
- Add `open_orders() -> Vec<Order>` to `ExchangeGateway` trait
- Implement in `BinanceGateway`: `GET /api/v3/openOrders`
- In `run_trading` CLI: call `gateway.open_orders()` after connect, re-register in `OrderManager`
- Query `orders` table for `status = 'submitted'` rows as cross-reference

---

## 🔲 4. Coinbase Gateway — Real Order Signing

`crates/gateway/src/coinbase.rs:41` — signing is a stub. All order calls are simulated.

**Work:**
- Implement HMAC-SHA256: `timestamp + method + path + body`
- Wire into `Authorization: Bearer` + `CB-ACCESS-KEY` / `CB-ACCESS-TIMESTAMP` / `CB-ACCESS-SIGN`
- Replace simulated `submit_order` / `cancel_order` with real HTTP POSTs to Advanced Trade API
- Subscribe to fill delivery via Coinbase WebSocket `user` channel

---

## 🔲 5e. InferenceStrategy stub

`crates/strategy/src/inference_strategy.rs` always emits no signals.

**Work:**
- Add `tract` or `ort` to `crates/strategy/Cargo.toml`
- Load an ONNX model path from config at startup
- Build a feature vector per book update: `[mid, spread, imbalance, rsi_14]`
- Threshold model output to generate buy/sell/flat signals

---

## 🔲 9. Performance — Reach Benchmark Targets

| Metric | Current | Target |
|---|---|---|
| `strategy/process_book_update` | ~222 ns | < 100 ns |
| `orderbook/apply_10_levels` | ~485 ns | < 200 ns |
| `event_bus/publish` | ~570 ns | < 200 ns |

**Work (in priority order):**

1. **OrderBook hot path** — `crates/core/src/orderbook.rs`
   - Replace `HashMap<Price, Level>` with two sorted `Vec<Level>` (bids desc, asks asc)
   - Binary search for insert/update/delete → avoids hash overhead on 20-level book
   - `apply_10_levels` target: < 200 ns

2. **Reduce `BookUpdate` copy size**
   - Currently 480 bytes per ring-buffer slot copy
   - Store levels as `[Level; 20]` with a length field (already done) — check if padding can be reduced
   - Consider `Arc<BookUpdate>` on the bus to avoid copying per subscriber

3. **Cache spread on `OrderBook`**
   - Avoid recomputing `mid_price()` / `spread_bps()` from scratch every call
   - Store `cached_mid: Option<Decimal>` updated only on `apply_update`

4. **`SO_TIMESTAMPING`** — wire-to-app latency from NIC
   - Add `cmsg` parsing in `crates/market/src/feed.rs` (Linux only)
   - Emit a `WireLatency` event payload so TUI can show NIC→strategy gap

---

## 🔲 10. Multi-Symbol Support

CLI accepts only `--symbol BTCUSDT`. Everything internally supports multi-symbol.

**Work:**
- Change `--symbol` to accept multiple values: `--symbol BTCUSDT --symbol ETHUSDT`
- `FeedManager::subscribe` already takes `Vec<String>` — pass all symbols in one call
  (Binance combined stream: `btcusdt@depth/ethusdt@depth`)
- `StrategyRunner` already dispatches by symbol — no change needed
- `RiskManager` positions are per-symbol — no change needed
- TUI: add a tab bar at the top; each tab shows one symbol's book + PnL

---

## ✅ 11. New Exchanges

All three exchanges fully implemented (feed parser + gateway + CLI routing):
- **OKX**: `OkxParser` + `OkxGateway` (HMAC-SHA256, base64 sig, passphrase header)
- **Bybit**: `BybitParser` + `BybitGateway` (HMAC-SHA256, X-BAPI-* headers)
- **Kraken**: `KrakenParser` + `KrakenGateway` (HMAC-SHA512 + SHA256, API-Sign)

---

## 🔲 12. Remaining Test Coverage

| Module | Status |
|---|---|
| `crates/execution/src/metrics.rs` | 0 tests |
| `crates/market/src/feed.rs` | 0 tests (reconnect logic) |
| `crates/gateway/src/coinbase.rs` | 0 tests |
| End-to-end pipeline test | missing |

**End-to-end test:**
- Record a small Parquet fixture (or use an existing one)
- In a `#[tokio::test]`: replay → `StrategyRunner` (MarketMaker) → `PaperGateway` → collect fills
- Assert: at least N fills, final position within limits, no panics

---

## Priority order for next session

1. **Crash recovery** (item 3) — highest operational risk, unblocked
2. **Coinbase signing** (item 4) — unlocks a second live exchange
3. **OrderBook performance** (item 9, step 1) — biggest remaining benchmark gap
4. **Multi-symbol** (item 10) — good UX improvement, mostly plumbing
5. **OKX feed + gateway** (item 11) — mechanical but adds real value
6. **End-to-end test** (item 12) — seals the test pyramid
7. **InferenceStrategy** (item 5e) — needs ONNX model, larger standalone task
