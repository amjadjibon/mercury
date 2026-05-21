# Gateways

Gateways implement the `ExchangeGateway` trait and handle order submission to exchanges.

## ExchangeGateway trait

```rust
#[async_trait]
pub trait ExchangeGateway: Send + Sync {
    async fn connect(&mut self) -> GatewayResult<()>;
    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId>;
    async fn cancel_order(&self, id: &OrderId, symbol: &Symbol) -> GatewayResult<()>;
    fn exchange(&self) -> Exchange;
}
```

## Supported gateways

| Gateway | Exchange | Notes |
|---------|----------|-------|
| `BinanceGateway` | Binance Spot | HMAC-SHA256 signing |
| `CoinbaseGateway` | Coinbase Advanced Trade | HMAC-SHA256 signing |
| `BybitGateway` | Bybit v5 | HMAC-SHA256 signing |
| `KrakenGateway` | Kraken v0 | HMAC-SHA512 with nonce |
| `OkxGateway` | OKX v5 | HMAC-SHA256, base64 |
| `PolymarketGateway` | Polymarket CLOB | HMAC-SHA256; fills via WebSocket user channel |
| `KalshiGateway` | Kalshi v2 | RSA-SHA256 private key signing |
| `PaperGateway` | — | Local simulated matching |

## PaperGateway

`PaperGateway` is a local matching engine that simulates order fills against live `BookUpdate` events on the bus. It is the recommended way to develop and test strategies.

- Default simulated RTT: **5 ms**
- No API keys required
- Enabled with `--paper` CLI flag

Configurable latency:

```rust
PaperGateway::with_latency(Duration::from_millis(10))
```

## Adding a new gateway

1. Create `crates/gateway/src/myexchange.rs`
2. Implement `ExchangeGateway`
3. Add the variant to `Exchange` enum in `mercury-core`
4. Register in `crates/cli/src/main.rs` (gateway match and feed_manager match)
