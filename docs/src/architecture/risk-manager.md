# Risk Manager

`RiskManager` enforces pre-trade risk controls. Every order passes through it before reaching the gateway.

## Checks

| Check | Description |
|-------|-------------|
| Position limit | Rejects orders that would exceed the configured max position size |
| Daily loss guard | Halts trading when realized + unrealized loss exceeds the daily limit |
| Rate limit | Enforces a maximum number of orders per second |
| Kill switch | Immediately blocks all new orders; drains open orders via cancel |

## Configuration

```rust
RiskConfig {
    max_position:    dec!(1.0),      // max open qty per symbol
    daily_loss_limit: dec!(500.0),   // USD
    max_orders_per_sec: 10,
}
```

## Kill switch

Activate programmatically:

```rust
risk_manager.kill();
```

Or via the TUI with the `K` keybinding. Once triggered, the engine stops submitting new orders and attempts to cancel all open positions.
