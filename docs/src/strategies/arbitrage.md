# Arbitrage Strategy

`ArbitrageStrategy` monitors the same symbol across two exchanges and trades when the price discrepancy exceeds transaction costs.

## Algorithm

1. Maintain best bid/ask for the symbol on both exchanges.
2. When `exchange_a.ask < exchange_b.bid - threshold`: buy on A, sell on B.
3. When `exchange_b.ask < exchange_a.bid - threshold`: buy on B, sell on A.
4. Block re-entry until both legs of the previous trade are filled.

## Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `min_spread` | `dec!(0.001)` | Minimum price discrepancy to trade |
| `order_qty` | `dec!(0.01)` | Order size per leg |

## Notes

True cross-exchange arbitrage requires near-simultaneous execution on both venues. Network latency between exchanges will erode the opportunity. This strategy is most useful in paper-trading mode to study spread behaviour.
