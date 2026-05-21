# TUI Monitor

`mercury-tui` is a standalone binary that connects to a running `mercury` process via a Unix socket and displays a real-time dashboard.

## Starting the TUI

```bash
cargo run --bin mercury-tui
```

The TUI connects to `/tmp/mercury.sock`. Start `mercury` first.

## Dashboard panels

- **Order book** — live L2 depth, colour-coded bid/ask
- **Fills** — scrolling list of recent fills with price, quantity, and exchange
- **PnL** — realized and unrealized PnL per symbol
- **Latency** — p50/p99/p999 for feed-to-signal and signal-to-fill
- **Orders** — open order list with status

## Keybindings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `K` | Activate kill switch (cancel all, halt trading) |
| `↑/↓` | Scroll fills / order list |
| `Tab` | Switch panel focus |

## IPC protocol

`mercury` broadcasts JSON-encoded `Event` values over `/tmp/mercury.sock`. The TUI deserializes them and updates its state. Multiple TUI instances can connect simultaneously.
