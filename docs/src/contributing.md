# Contributing

## Development setup

```bash
git clone https://github.com/amjadjibon/mercury
cd mercury
cargo build --workspace
cargo test --workspace
```

## Code conventions

- **No `f64` for financial values** — use `rust_decimal::Decimal` and the `dec!()` macro.
- **`Symbol` is 16 bytes** — strings longer than 16 bytes are silently truncated.
- **No heap allocation on the hot path** — prefer stack types in `on_book` and `on_trade`.
- **Comments only when the _why_ is non-obvious** — well-named identifiers document the what.

## Running the docs locally

```bash
cargo install mdbook
mdbook serve docs --open
```

## Submitting changes

1. Fork the repository and create a feature branch.
2. Run `cargo test --workspace` and `cargo build --workspace` — both must pass with zero warnings.
3. Open a pull request with a description of what changed and why.
