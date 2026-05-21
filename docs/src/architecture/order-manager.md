# Order Manager

`OrderManager` sits between strategies and the exchange gateway. It applies risk checks before forwarding orders and tracks their lifecycle.

## Responsibilities

1. Receive `Signal` events from the `EventBus`.
2. Convert signals to `Order` values.
3. Call `RiskManager::check` — reject orders that breach limits.
4. Submit approved orders via `ExchangeGateway::submit_order`.
5. Track open orders by `OrderId`.
6. Publish `Fill` events back onto the `EventBus` when acknowledgements arrive.

## Smart order routing

`SmartOrderRouter` wraps multiple gateways and selects the venue with the best available price for the given symbol and side. It is opt-in; the default configuration uses a single gateway.

## Execution algorithms

Large parent orders can be worked using algorithmic executors. Each executor runs as a background `tokio` task and emits child `Signal` events onto the `EventBus`, which `OrderManager` receives and processes normally.

| Executor | Description |
|----------|-------------|
| `TwapExecutor` | Splits a parent order into `N` equal-sized slices emitted at uniform time intervals (time-weighted average price). |
| `VwapExecutor` | Sizes each slice in proportion to observed market trade volume for the interval; falls back to equal-weight when no volume is seen. |
| `PovExecutor` | Participates as a configurable percentage of observed market volume, bounded by min/max slice sizes, with a wall-clock deadline that flushes any residual quantity. |

### Usage

```rust
use mercury_execution::{TwapExecutor, VwapExecutor, PovExecutor};

// TWAP — 10 equal slices over 5 minutes
TwapExecutor::new(bus.clone()).execute(signal.clone(), 300, 10);

// VWAP — 8 volume-proportional slices over 5 minutes
VwapExecutor::new(bus.clone()).execute(signal.clone(), 300, 8);

// POV — 10% participation, min 0.01 / max 1.0 per slice, 10-minute deadline
PovExecutor::new(bus.clone()).execute(
    signal,
    0.10,                                    // participation_rate
    FixedPoint::from_decimal(dec!(0.01)),    // min_slice_qty
    FixedPoint::from_decimal(dec!(1.0)),     // max_slice_qty
    600,                                     // deadline_secs
);
```

All three executors observe `Trade` events on the `EventBus` to measure market volume. No additional configuration is required beyond passing the shared `Arc<EventBus>`.
