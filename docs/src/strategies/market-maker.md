# Market Maker

`MarketMaker` posts symmetric limit orders on both sides of the mid-price and adjusts quotes on every book update.

## Algorithm

1. Compute `mid = (best_bid + best_ask) / 2`.
2. Apply inventory skew: if long, widen the ask and tighten the bid to mean-revert.
3. Place a bid at `mid - spread/2` and an ask at `mid + spread/2`.
4. On the next book update, cancel stale quotes and replace with fresh ones (peg mode reduces churn by only replacing when price moves beyond a threshold).

## Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `spread` | `dec!(0.002)` | Total spread as a fraction of mid |
| `order_qty` | `dec!(0.01)` | Order size in base currency |
| `inventory_skew` | `dec!(0.5)` | Multiplier for inventory-based skew |
| `peg_threshold` | `dec!(0.0005)` | Min price move before requoting (peg mode) |

## Peg mode

Peg mode (enabled by default) avoids unnecessary cancel/replace cycles. A quote is only refreshed when the mid-price moves more than `peg_threshold` from the last quoted price. This reduces exchange fees and order rate usage.
