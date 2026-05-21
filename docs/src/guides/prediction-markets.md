# Prediction Markets

Mercury supports [Polymarket](https://polymarket.com) (CLOB-based) and [Kalshi](https://kalshi.com) (regulated US) alongside crypto exchanges. Binary outcome contracts (YES/NO shares) map directly onto the existing `OrderBook`, `BookUpdate`, and `Fill` types — prices are stored as probabilities in `FixedPoint` (e.g. `0.42` = 42% probability).

## Symbol derivation

### Polymarket

Polymarket identifies markets by a 64-character hex `condition_id`. Because `Symbol` is 16 bytes, Mercury derives a stable key using the **last 16 characters** of the condition ID:

```
condition_id: 0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af
Symbol key:                                                  0ca09bd4532f84af
```

The low-order bytes of the uint256 are unique across all active markets, so no collision occurs in practice. The helper `condition_id_to_symbol(cid)` in `mercury-market` performs this mapping.

### Kalshi

Kalshi market tickers (e.g. `TRUMPWIN-2024`) are ≤ 15 characters and fit directly in `Symbol` with no truncation.

---

## Polymarket

### Feed (market data)

`PolymarketParser` subscribes to the Polymarket CLOB WebSocket:

- **URL**: `wss://ws-subscriptions-clob.polymarket.com/ws/market`
- **Subscription**: `{"assets_ids": ["<condition_id>"], "type": "market"}`
- **Messages**: `book` (full snapshot) → `DepthSnapshot`; `price_change` (delta) → `DepthUpdate`

### Gateway (order execution)

`PolymarketGateway` uses the Polymarket CLOB REST API (`https://clob.polymarket.com`):

| Operation | Endpoint |
|-----------|----------|
| Submit order | `POST /order` |
| Cancel order | `DELETE /orders/{order_id}` |
| Cancel all | `DELETE /orders` |
| Fills | WebSocket user channel `wss://ws-subscriptions-clob.polymarket.com/ws/user` |

**Authentication**: HMAC-SHA256 over `{timestamp}{nonce}POST{body}` using `api_secret`.

### Configuration

```rust
PolymarketConfig {
    api_key:        String,   // POLY-API-KEY header
    api_secret:     String,   // HMAC signing key
    api_passphrase: String,   // POLY-PASSPHRASE header
}
```

Environment variables: `EXCHANGE_API_KEY`, `EXCHANGE_SECRET_KEY`, `POLYMARKET_PASSPHRASE`.

### Usage

```bash
# Paper trade (live feed, simulated orders)
cargo run --bin mercury -- run \
  --exchange polymarket \
  --symbol 0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af \
  --paper \
  --strategy momentum

# Live trading
EXCHANGE_API_KEY=your_api_key \
EXCHANGE_SECRET_KEY=your_api_secret \
POLYMARKET_PASSPHRASE=your_passphrase \
  cargo run --bin mercury -- run \
  --exchange polymarket \
  --symbol 0x...condition_id... \
  --strategy rsi
```

---

## Kalshi

### Feed (market data)

`KalshiParser` subscribes to the Kalshi WebSocket:

- **URL**: `wss://api.elections.kalshi.com/trade-api/ws/v2`
- **Subscription**: `{"cmd": "subscribe", "params": {"channels": ["orderbook_delta"], "market_tickers": ["TICKER"]}}`
- **Messages**: `orderbook_snapshot` → `DepthSnapshot`; `orderbook_delta` → `DepthUpdate`
- **Prices**: Kalshi sends integer cents (e.g. `42` = 42 cents = 0.42 probability). The parser converts to `FixedPoint` automatically.

### Gateway (order execution)

`KalshiGateway` uses the Kalshi REST API (`https://api.elections.kalshi.com/trade-api/v2`):

| Operation | Endpoint |
|-----------|----------|
| Submit order | `POST /portfolio/orders` |
| Cancel order | `DELETE /portfolio/orders/{order_id}` |
| Cancel all | `DELETE /portfolio/orders?ticker={ticker}` |
| Fills | WebSocket `fills` channel |

**Authentication**: RSA-SHA256 (PKCS#1v1.5). Signature message: `{timestamp_ms}{method}{path}`. The signed bytes are base64-encoded and sent in the `KALSHI-ACCESS-SIGNATURE` header.

### Configuration

```rust
KalshiConfig {
    api_key_id:      String,   // UUID from the Kalshi dashboard
    private_key_pem: String,   // PEM-encoded RSA private key
}
```

Environment variables: `KALSHI_API_KEY_ID`, `KALSHI_PRIVATE_KEY` (PEM string or path to a `.pem` file).

### Usage

```bash
# Paper trade
cargo run --bin mercury -- run \
  --exchange kalshi \
  --symbol TRUMPWIN-2024 \
  --paper \
  --strategy rsi

# Live trading — pass PEM file path
KALSHI_API_KEY_ID=your-uuid \
KALSHI_PRIVATE_KEY=./kalshi_private.pem \
  cargo run --bin mercury -- run \
  --exchange kalshi \
  --symbol TRUMPWIN-2024 \
  --strategy rsi
```

---

## Price representation

Both markets price YES shares between 0 and 1 (in cents: 0–100). `FixedPoint` handles this with 8 decimal places of precision — no floating-point rounding errors.

```
Kalshi:     42 cents  →  parser converts to  0.42  FixedPoint
Polymarket: "0.42"    →  FixedPoint::from_str("0.42")
```

`Side::Buy` means buying YES shares. `Side::Sell` means selling YES shares (equivalent to buying NO at `1 - price`).
