# Strategies Overview

Strategies implement the `Strategy` trait and are driven by the `StrategyRunner`.

## Strategy trait

```rust
pub trait Strategy: Send + Sync {
    fn on_book(&mut self, book: &Arc<BookUpdate>) -> Vec<Signal>;
    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal>;
    fn on_fill(&mut self, fill: &Fill);
    fn reset(&mut self);
    fn name(&self) -> &'static str;
}
```

- `on_book` — called on every order-book snapshot; return signals to trade.
- `on_trade` — called on every tape print.
- `on_fill` — called when one of this strategy's orders is filled; use to update inventory.
- `reset` — clear all state (used before backtests and replays).

## Built-in strategies

| Name | Flag | Description |
|------|------|-------------|
| `MarketMaker` | `market_maker` | Symmetric quotes with inventory skew and HMM regime gating (2× spread when trending, 0.75× when mean-reverting) |
| `Momentum` | `momentum` | Trend-following using EMA crossover |
| `RsiStrategy` | `rsi` | Mean-reversion using RSI overbought/oversold |
| `ArbitrageStrategy` | `arbitrage` | Cross-exchange price discrepancy |
| `InferenceStrategy` | `inference` | ONNX or online Adam-SGD classifier on 12-feature vector with Welford z-score normalisation |
| `RlQuotePlacementStrategy` | `rl` | DQN quote placement; 7-state input adds HMM trend probability alongside 6 market features |

## Technical indicators

All indicators live in `mercury-strategy` and operate on `Decimal` values:

- `SMA(period)` — simple moving average
- `EMA(period)` — exponential moving average
- `RSI(period)` — relative strength index
- `MACD(fast, slow, signal)` — moving average convergence/divergence
