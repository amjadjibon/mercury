# Market Maker

`MarketMaker` posts symmetric limit orders on both sides of the mid-price and adjusts quotes on every book update.

## Algorithm

1. Compute `mid = (best_bid + best_ask) / 2`.
2. Feed the mid price into the **HMM regime filter** and apply a spread multiplier based on the detected regime.
3. Apply inventory skew: if long, widen the ask and tighten the bid to mean-revert.
4. Place a bid at `mid - half_spread` and an ask at `mid + half_spread`.
5. On the next book update, cancel stale quotes and replace with fresh ones (peg mode reduces churn by only replacing when price moves beyond a threshold).

## Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `spread` | `dec!(0.002)` | Base total spread as a fraction of mid |
| `order_qty` | `dec!(0.01)` | Order size in base currency |
| `inventory_skew` | `dec!(0.5)` | Multiplier for inventory-based skew |
| `peg_threshold` | `dec!(0.0005)` | Min price move before requoting (peg mode) |

## Regime-gated spread scaling

`MarketMaker` embeds an `HmmFilter` (two-state hidden Markov model) that classifies the market into `Trending` or `MeanReverting` based on the sign persistence of consecutive log returns.

| Regime | Spread multiplier | Rationale |
|--------|-------------------|-----------|
| Trending | **2.0×** | Informed directional flow → widen to reduce adverse selection |
| MeanReverting | **0.75×** | Noise-driven oscillation → tighten to capture more fills |
| Uncertain (< 0.60 confidence) | **1.0×** | Default spread, no adjustment |

The HMM needs at least two price updates before emitting a confident regime. During warm-up the multiplier is 1.0.

Customise via `RegimeConfig`:

```rust
use mercury_strategy::{MarketMaker, RegimeConfig, HmmConfig};

let regime = RegimeConfig {
    hmm: HmmConfig {
        stay_probability: 0.95,
        confidence_threshold: 0.65,  // require higher confidence before scaling
        ..HmmConfig::default()
    },
    trending_spread_mult: 2.5,        // even wider in strong trends
    mean_revert_spread_mult: 0.80,
};

let mm = MarketMaker::new("BTCUSDT", dec!(0.002), dec!(0.01))
    .with_regime_config(regime);
```

## Peg mode

Peg mode (enabled by default) avoids unnecessary cancel/replace cycles. A quote is only refreshed when the mid-price moves more than `peg_threshold` from the last quoted price. This reduces exchange fees and order rate usage.
