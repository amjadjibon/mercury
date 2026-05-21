# Writing a Custom Strategy

## 1. Create the file

Add `crates/strategy/src/my_strategy.rs` and implement the `Strategy` trait:

```rust
use std::sync::Arc;
use mercury_core::{BookUpdate, Fill, Signal, Trade};
use crate::Strategy;

pub struct MyStrategy {
    // your state here
}

impl MyStrategy {
    pub fn new() -> Self {
        Self {}
    }
}

impl Strategy for MyStrategy {
    fn name(&self) -> &'static str { "my_strategy" }

    fn on_book(&mut self, book: &Arc<BookUpdate>) -> Vec<Signal> {
        // compute signals from the order book
        vec![]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {
        // update inventory / reinforce learning
    }

    fn reset(&mut self) {
        // clear all state
        *self = Self::new();
    }
}
```

## 2. Export from the crate

In `crates/strategy/src/lib.rs`:

```rust
pub mod my_strategy;
pub use my_strategy::MyStrategy;
```

## 3. Register the CLI flag

In `crates/cli/src/run.rs`, add a match arm for the new strategy name:

```rust
"my_strategy" => Box::new(MyStrategy::new()),
```

## 4. Run it

```bash
cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy my_strategy
```

## Tips

- Keep `on_book` fast — it is called on every tick. Avoid allocations on the hot path.
- Use `rust_decimal::Decimal` for all price/quantity arithmetic.
- Initialise warm-up state in `reset()` so backtests start clean.
- Use `on_fill` to track inventory rather than counting signals, since orders may be partially filled or rejected.
