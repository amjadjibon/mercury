# RSI Strategy

`RsiStrategy` is a mean-reversion strategy based on the Relative Strength Index.

## Algorithm

1. Compute RSI over the last `period` trade prices (default 14).
2. When RSI drops below `oversold` (default 30): emit a **buy** signal.
3. When RSI rises above `overbought` (default 70): emit a **sell** signal.
4. No signal is emitted until the RSI has warmed up.

## Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `period` | `14` | RSI lookback period |
| `oversold` | `30` | RSI level that triggers a buy |
| `overbought` | `70` | RSI level that triggers a sell |
| `order_qty` | `dec!(0.01)` | Order size per signal |
