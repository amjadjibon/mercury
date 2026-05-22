# ML Inference Strategy

`InferenceStrategy` generates buy/sell signals from a 12-feature order-book vector using either a pre-trained ONNX model or an online Adam-SGD classifier that trains itself in real-time.

## How it works

1. On every `on_book` call, `FeatureComputer` builds a 12-element feature vector from the current book snapshot and recent trade history.
2. `RunningNormalizer` standardises the vector to approximately zero mean / unit variance (active after a 30-tick warm-up; raw features are used before that).
3. The normalised vector is either:
   - **ONNX path** — fed to a `tract-onnx` plan loaded from disk.
   - **Online fallback** — classified by an incremental 3-class softmax trained with Adam via delayed self-labelling (label assigned after `lookahead_ticks` ticks using mid-price movement).
4. `[sell_prob, hold_prob, buy_prob]` drives a threshold gate (0.65 for ONNX, 0.60 for online); a signal is emitted only when one class exceeds the threshold.

## Feature vector (12 features)

| Index | Feature | Range |
|-------|---------|-------|
| 0 | Order imbalance `(bid_qty − ask_qty) / total` | [−1, 1] |
| 1 | Spread bps / 100 | [0, ∞) |
| 2 | RSI-14 / 100 | [0, 1] |
| 3 | MACD histogram (sign × clamped magnitude) | [−1, 1] |
| 4 | Depth ratio top-5 bid / total | [0, 1] |
| 5 | EMA-50 deviation `(mid − ema) / ema` | uncapped |
| 6 | ATR-14 / mid | [0, ∞) |
| 7 | VWAP deviation, 100-trade rolling | uncapped |
| 8 | Depth slope `(bid[0] − bid[4]) / mid` | uncapped |
| 9 | Trade-flow imbalance, 20-trade rolling | [0, 1] |
| 10 | Kyle's Lambda (tanh-squashed price-impact coeff) | (−1, 1) |
| 11 | VPIN trade toxicity | [0, 1] |

`compute_extended()` appends a 13th feature (index 12): Hawkes trade-arrival intensity.

## Online normalisation

`RunningNormalizer` runs Welford's one-pass algorithm to accumulate per-feature mean and variance. Features are normalised in-place before reaching the classifier:

```
z_i = (x_i − μ_i) / max(σ_i, 1e-8)
```

Normalisation activates after 30 observations. Before warm-up, raw features are used unchanged.

## Online learning (no ONNX model)

When no model file is supplied, the strategy trains an in-process softmax classifier with the **Adam optimizer** (β₁ = 0.9, β₂ = 0.999, ε = 1e-8, lr = 0.001, L2 = 1e-4):

- Each tick the feature vector is stored alongside the current mid price.
- After `lookahead_ticks` ticks, the mid-price change determines the label: `+1` (price rose > threshold), `-1` (fell), `0` (flat).
- An Adam gradient step updates the 3-class weight matrix `(3 × 12)`.
- Signals are emitted only after `min_updates_before_signal` updates (default 25) to avoid noise during cold-start.

```rust
let config = OnlineLearningConfig {
    lookahead_ticks: 10,
    threshold: 0.0005,           // 0.05% price move to assign a label
    min_updates_before_signal: 25,
    classifier: OnlineClassifierConfig {
        learning_rate: 0.001,
        l2: 0.0001,
        beta1: 0.9,
        beta2: 0.999,
        epsilon: 1e-8,
    },
};
let strat = InferenceStrategy::new("BTCUSDT", dec!(0.01), None, Some(bus))
    .with_online_config(config);
```

## Loading an ONNX model

```bash
cargo run --bin mercury -- run \
  --symbol BTCUSDT \
  --paper \
  --strategy inference \
  --model path/to/model.onnx
```

The expected ONNX input shape is `[1, 12]` with output shape `[1, 3]` (softmax over [SELL, HOLD, BUY]). A `[1, 2, 20]` LOB CNN model is also supported via `InferenceStrategy::new_lob_cnn`.

## Model checkpointing

Online classifier weights can be saved and restored between sessions:

```rust
// Save after a live session
clf.save("btcusdt_weights.json")?;

// Restore at startup
let mut clf = OnlineClassifier::new(OnlineClassifierConfig::default());
clf.load("btcusdt_weights.json")?;
```

The checkpoint format is compact JSON containing `updates`, `bias[3]`, and `weights[36]` (3 classes × 12 features).
