# Momentum

`Momentum` is a trend-following strategy that uses an EMA crossover to generate directional signals.

## Algorithm

1. Maintain a fast EMA (default period 12) and a slow EMA (default period 26) over trade prices.
2. When fast EMA crosses above slow EMA: emit a **buy** signal.
3. When fast EMA crosses below slow EMA: emit a **sell** signal.
4. No signal is emitted until both EMAs have warmed up (require `slow_period` trades).

## Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `fast_period` | `12` | Fast EMA period |
| `slow_period` | `26` | Slow EMA period |
| `order_qty` | `dec!(0.01)` | Order size per signal |
