# Installation

## Prerequisites

- Rust toolchain ≥ 1.85 (edition 2024) — install via [rustup](https://rustup.rs)
- SQLite (system library, used by sqlx)
- OpenSSL or native-tls (used by reqwest / tokio-tungstenite)

On macOS:
```bash
brew install openssl sqlite
```

On Ubuntu/Debian:
```bash
apt install libssl-dev libsqlite3-dev pkg-config
```

## Build

```bash
git clone https://github.com/amjadjibon/mercury
cd mercury
cargo build --workspace --release
```

## Dev tools

The following tools are used for documentation and benchmarking. Install them once:

```bash
cargo install mdbook           # build this book
cargo install cargo-criterion  # benchmark runner
```

## Verify

```bash
cargo test --workspace
```

All tests should pass with no warnings.
