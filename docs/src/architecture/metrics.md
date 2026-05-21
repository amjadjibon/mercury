# Metrics

`mercury-metrics` provides latency histograms, PnL tracking, and a Prometheus export endpoint.

## LatencyTracker

Uses HDR histograms (via `hdrhistogram`) to record latency with microsecond precision across several orders of magnitude.

Key measurements:

- **Feed-to-strategy**: time from `BookUpdate` publish to `on_book` return
- **Signal-to-submit**: time from `Signal` publish to `submit_order` call
- **Submit-to-fill**: exchange round-trip time

Access percentiles:

```rust
let p99 = tracker.percentile(99.0); // nanoseconds
let p999 = tracker.percentile(99.9);
```

## PnlTracker

Tracks realized and unrealized PnL per symbol:

```rust
tracker.on_fill(&fill);
let pnl = tracker.unrealized_pnl(&symbol, current_mid_price);
```

## Prometheus

Metrics are exported on `0.0.0.0:9090/metrics` when `--metrics` is passed. Compatible with any Prometheus scrape target.

Available metrics:

- `mercury_fill_latency_ns` — histogram
- `mercury_pnl_realized_usd` — gauge per symbol
- `mercury_orders_submitted_total` — counter
- `mercury_orders_cancelled_total` — counter
- `mercury_risk_rejections_total` — counter
