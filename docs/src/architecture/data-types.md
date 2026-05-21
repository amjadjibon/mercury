# Data Types

## Decimal arithmetic

All prices and quantities use `rust_decimal::Decimal`. Never use `f64` for financial values — floating-point rounding errors compound over time and can cause incorrect P&L calculations.

Use the `dec!()` macro for literals:

```rust
use rust_decimal_macros::dec;

let price = dec!(42_150.50);
let qty   = dec!(0.001);
let notional = price * qty; // Decimal, exact
```

## Symbol

`Symbol` is a fixed-size `[u8; 16]` stack array. This avoids heap allocation on the hot path.

```rust
let sym: Symbol = Symbol::new("BTCUSDT");
let sym: Symbol = "BTCUSDT".into(); // same thing
```

Strings longer than 16 bytes are silently truncated. Keep symbol names short.

## Order

```rust
pub struct Order {
    pub id:            OrderId,
    pub symbol:        Symbol,
    pub exchange:      Exchange,
    pub side:          Side,        // Bid | Ask
    pub order_type:    OrderType,   // Limit | Market | PostOnly
    pub price:         Decimal,
    pub quantity:      Decimal,
    pub time_in_force: TimeInForce, // GTC | IOC | FOK | PostOnly
}
```

## Fill

```rust
pub struct Fill {
    pub order_id:   OrderId,
    pub symbol:     Symbol,
    pub exchange:   Exchange,
    pub side:       Side,
    pub price:      Decimal,
    pub quantity:   Decimal,
    pub fee:        Decimal,
    pub timestamp:  i64,  // Unix millis
}
```

## Signal

Strategies emit `Signal` values that the `OrderManager` converts into orders:

```rust
pub struct Signal {
    pub symbol:    Symbol,
    pub side:      Side,
    pub price:     Decimal,
    pub quantity:  Decimal,
    pub strategy:  &'static str,
}
```
