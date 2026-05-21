# Adding a New Exchange

Adding exchange support requires implementing two traits: `FeedParser` (market data) and `ExchangeGateway` (order routing).

## 1. Market feed

Create `crates/market/src/myexchange.rs`:

```rust
pub struct MyExchangeParser;

impl FeedParser for MyExchangeParser {
    fn ws_url(&self, symbol: &Symbol) -> String {
        format!("wss://stream.myexchange.com/ws/{}", symbol)
    }

    fn subscribe_message(&self, symbol: &Symbol) -> String {
        serde_json::json!({ "op": "subscribe", "channel": symbol.to_string() }).to_string()
    }

    fn parse(&self, msg: &str) -> Vec<EventPayload> {
        // deserialise msg and return BookUpdate / Trade events
        vec![]
    }
}
```

## 2. Gateway

Create `crates/gateway/src/myexchange.rs` and implement `ExchangeGateway`.

See `crates/gateway/src/binance.rs` as a reference for HMAC signing, REST error handling, and order ID mapping.

## 3. Add the Exchange variant

In `crates/core/src/types.rs`, add to the `Exchange` enum:

```rust
pub enum Exchange {
    Binance,
    Coinbase,
    MyExchange, // add this
    // ...
}
```

## 4. Wire up in the CLI

In `crates/cli/src/run.rs`, add a match arm for `"myexchange"` that constructs your feed parser and gateway.

## 5. Test

Write unit tests for `parse()` with real WebSocket message payloads captured from the exchange. Use `cargo test -p mercury-market myexchange` to run them.
