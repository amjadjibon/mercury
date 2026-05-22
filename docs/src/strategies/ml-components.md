# ML Components

Mercury's strategy crate ships self-contained machine-learning building blocks — no Python, no external service, all pure Rust.

---

## Feature engineering (`FeatureComputer`)

`FeatureComputer` maintains running indicator state and produces a **12-element `f32` vector** on every `on_book` call.

| Index | Feature | Range | Notes |
|-------|---------|-------|-------|
| 0 | Order imbalance | [−1, 1] | `(bid_qty − ask_qty) / total` |
| 1 | Spread bps / 100 | [0, ∞) | |
| 2 | RSI-14 / 100 | [0, 1] | |
| 3 | MACD histogram | [−1, 1] | sign × clamped magnitude |
| 4 | Depth ratio top-5 | [0, 1] | bid depth fraction |
| 5 | EMA-50 deviation | uncapped | `(mid − ema) / ema` |
| 6 | ATR-14 / mid | [0, ∞) | |
| 7 | VWAP deviation | uncapped | 100-trade rolling |
| 8 | Depth slope | uncapped | `(bid[0] − bid[4]) / mid` |
| 9 | Trade-flow imbalance | [0, 1] | 20-trade rolling buy fraction |
| 10 | **Kyle's Lambda** | (−1, 1) | `tanh(λ × 1000)` — price-impact coefficient |
| 11 | **VPIN** | [0, 1] | volume-synchronised toxicity |

`compute_extended()` appends a 13th feature at index 12: bounded Hawkes trade-arrival intensity.

---

## Online normalisation (`RunningNormalizer`)

Implements Welford's one-pass algorithm to track per-feature mean and variance without storing history.

```rust
pub struct RunningNormalizer {
    mean: [f64; FEATURE_COUNT],
    m2:   [f64; FEATURE_COUNT],   // Welford M2 accumulator
    count: u64,
    min_count: u64,               // warm-up ticks before normalisation activates
}
```

Features are standardised in-place: `z_i = (x_i − μ_i) / max(σ_i, 1e-8)`.
Before `min_count` observations, features are returned unchanged.

`InferenceStrategy` uses a 30-tick warm-up by default.

---

## Adam optimizer (`OnlineClassifier`)

3-class softmax logistic regression trained with the Adam adaptive gradient method.

| Hyperparameter | Default | Description |
|----------------|---------|-------------|
| `learning_rate` | 0.001 | Adam α |
| `beta1` | 0.9 | First-moment decay |
| `beta2` | 0.999 | Second-moment decay |
| `epsilon` | 1e-8 | Numerical stability |
| `l2` | 1e-4 | L2 weight decay |

Update rule (per weight):
```
m_t = β₁·m_{t-1} + (1−β₁)·g
v_t = β₂·v_{t-1} + (1−β₂)·g²
α_t = α · √(1−β₂ᵗ) / (1−β₁ᵗ)
w ← w − α_t · m̂_t / (√v̂_t + ε)
```

---

## HMM regime filter (`HmmFilter`)

Two-state online forward filter classifying the market into `Trending` or `MeanReverting`.

**Observation**: sign persistence of consecutive log returns (+1 same direction, −1 alternating).

**States**:
- `Trending` — emission mean +0.75 (returns persist in the same direction).
- `MeanReverting` — emission mean −0.75 (returns alternate sign).

```rust
let mut hmm = HmmFilter::default();
hmm.update_price(50_000.0);  // 1st price — no output
hmm.update_price(50_050.0);  // 2nd — computes 1st return, no output
let regime = hmm.update_price(50_100.0);  // 3rd — first regime estimate
// Some(Regime::Trending)

let trend_prob = hmm.trend_probability();  // ∈ [0, 1]
```

A confident regime is declared only when `trend_probability ≥ confidence_threshold` (default 0.60).

**Integration points**:
- `MarketMaker` — spread multiplier: 2.0× trending, 0.75× mean-reverting.
- `RlQuotePlacementStrategy` — `trend_probability − 0.5` as state dimension 6.

---

## Microstructure estimators (`microstructure.rs`)

### Kyle's Lambda

Price-impact coefficient estimated via rolling OLS on signed order flow:

```
λ = Σ(Q_i · ΔP_i) / Σ(Q_i²)
```

where Q is signed volume (+buy, −sell) and ΔP is the concurrent mid-price change.
Rising λ indicates informed directional flow.

```rust
let mut kl = KyleLambda::new(20);         // 20-trade rolling window
kl.on_trade(1.0, Side::Buy, 0.01);        // qty, side, price_change
let lambda = kl.lambda();                  // Some(f64) once window is full
```

### Roll Spread

Estimates the effective half-spread from the serial covariance of price changes (Roll, 1984):

```
c = √(−Cov(ΔP_t, ΔP_{t−1}))
```

Valid only when serial covariance is negative (as expected when bid-ask bounce dominates).

```rust
let mut rs = RollSpread::new(20);
rs.on_price(100.0);
let half_spread = rs.half_spread();       // Some(f64) once window full and Cov < 0
```

### VPIN

Volume-synchronised probability of informed trading (Easley et al., 2012):

- Volume is divided into fixed-size buckets.
- Each bucket: `vpin = |buy_vol − sell_vol| / bucket_size`.
- Rolling average over the last `window_buckets` gives VPIN ∈ [0, 1].
- Values > 0.5 indicate a toxic order-flow environment.

```rust
let mut vpin = Vpin::new(10.0, 20);       // bucket_size=10, 20-bucket window
vpin.on_trade(1.0, Side::Buy);
let score = vpin.vpin();                   // 0.0 until first bucket completes
```

---

## Model checkpointing (`checkpoint.rs`)

### Online classifier (JSON)

```rust
// Save weights + bias + update count.
clf.save("model.json")?;

// Restore.
let mut clf = OnlineClassifier::new(OnlineClassifierConfig::default());
clf.load("model.json")?;
```

Format: `{"updates": u64, "bias": [f32; 3], "weights": [f32; 36]}` (3 × 12 features).

### DQN network (binary f32 LE)

```rust
// Flatten each weight matrix then save as one binary blob.
save_f32_weights("dqn.bin", &[&w1_flat, &w2_flat, &w3_flat])?;

// Load and split by known sizes.
let weights = load_f32_weights("dqn.bin", total_params)?;
```

Dimension mismatch on load returns `io::Error::InvalidData`.
