# Market Feeds

The `mercury-market` crate manages all inbound market data.

## FeedManager

`FeedManager` owns one or more WebSocket connections and routes parsed messages onto the `EventBus`. It handles reconnection automatically on disconnect.

## Implementing a new feed

Implement the `FeedParser` trait:

```rust
pub trait FeedParser: Send + Sync {
    /// Return the WebSocket URL for this exchange/symbol.
    fn ws_url(&self, symbol: &Symbol) -> String;

    /// Return the JSON subscription message to send after connecting.
    fn subscribe_message(&self, symbol: &Symbol) -> String;

    /// Parse a raw WebSocket message into zero or more events.
    fn parse(&self, msg: &str) -> Vec<EventPayload>;
}
```

Once implemented, register your parser with `FeedManager::add_feed`.

## Supported feeds

| Parser | Exchange | Data |
|--------|----------|------|
| `BinanceParser` | Binance | Order book depth (L2), trades |
| `CoinbaseParser` | Coinbase | Order book depth (L2), trades |
| `YahooFeed` | Yahoo Finance | OHLCV bars (HTTP polling) |

## BookBuilder

`BookBuilder` maintains a full L2 order book from incremental `BookUpdate` events. It applies sequence-numbered deltas and publishes a fresh `Arc<BookUpdate>` snapshot after each delta is applied.
