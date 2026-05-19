# Repository Guidelines

## Project Structure & Module Organization

Mercury is a Rust workspace (`Cargo.toml`) organized under `crates/`. Core trading primitives live in `crates/core`; feeds and parsers in `crates/market`; strategies in `crates/strategy`; risk in `crates/risk`; execution/backtesting in `crates/execution`; exchange adapters in `crates/gateway`; replay in `crates/replay`; metrics in `crates/metrics`; persistence in `crates/storage`; binaries in `crates/cli` and `crates/tui`. Benchmarks are in `crates/benches/benches`; SQL migrations are in `crates/storage/migrations`. `ticks.parquet` is sample market data.

## Build, Test, and Development Commands

- `cargo build --workspace`: build every crate and binary.
- `cargo test --workspace`: run all unit and async tests.
- `cargo fmt --all`: format Rust code before review.
- `cargo clippy --workspace --all-targets`: lint libraries, binaries, tests, and benches.
- `cargo run --bin mercury -- run --symbol BTCUSDT --paper --strategy market_maker`: start paper trading without API keys.
- `cargo run --bin mercury -- record --symbol BTCUSDT --output data.parquet`: record live ticks to Parquet.
- `cargo run --bin mercury-tui`: launch the terminal monitor for a running engine.
- `cargo bench`: run Criterion performance benchmarks.

## Coding Style & Naming Conventions

Use Rust 2024 edition idioms and keep code formatted with `rustfmt`. Module and file names use `snake_case`; public types and traits use `UpperCamelCase`; constants use `SCREAMING_SNAKE_CASE`. Prefer workspace dependencies in the root `Cargo.toml` over duplicating versions in crate manifests. Hot-path code should preserve the existing low-allocation style: fixed-size arrays, explicit data structures, and no avoidable heap allocation.

## Testing Guidelines

Most tests are inline `#[cfg(test)] mod tests` blocks next to the implementation they cover. Use `#[test]` for synchronous logic and `#[tokio::test]` for async flows. Name tests by behavior, for example `generates_buy_signal_when_rsi_oversold`. Add focused tests when changing parsers, strategies, risk checks, order routing, replay, or fixed-point/order book behavior. Run `cargo test --workspace` before submitting changes; run `cargo bench` when changing hot-path or allocation-sensitive code.

## Commit & Pull Request Guidelines

Recent history uses short imperative subjects, often Conventional Commit prefixes such as `feat:`, `refactor`, and `chore:`. Keep subjects specific, for example `feat: add OBI strategy tests`. Pull requests should describe the behavior change, list validation commands, link issues, and include screenshots only for TUI-visible changes. Call out config, schema, migration, or live-trading risk changes explicitly.

## Security & Configuration Tips

Do not commit API keys, secrets, or private `mercury.toml` files. Prefer environment variables such as `BINANCE_API_KEY` and `BINANCE_SECRET_KEY` for live trading. Default to `--paper` during local validation unless a task explicitly requires live exchange access.
