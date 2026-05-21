# Paper Trading

Paper trading runs Mercury against a live market feed but routes all orders through `PaperGateway` — a local matching engine. No real money is at risk.

## Enable paper trading

```bash
cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy market_maker
```

## How PaperGateway works

1. Receives live `BookUpdate` events from the market feed.
2. When a new order arrives, checks whether it crosses the current best bid/ask.
3. If it fills, publishes a synthetic `Fill` event onto the `EventBus` after a configurable simulated delay.
4. Default simulated round-trip latency: **5 ms**.

## Custom latency

The paper gateway latency is set in code:

```rust
// crates/cli/src/run.rs
let gateway = PaperGateway::with_latency(Duration::from_millis(10));
```

## Limitations

- No partial fills — orders either fill completely or not at all.
- No queue position — the paper gateway assumes your order is at the top of the queue.
- Simulated latency is constant; real latency is variable.

These simplifications make paper results optimistic. Use backtesting on recorded data for more realistic simulation.
