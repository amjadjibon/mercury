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
