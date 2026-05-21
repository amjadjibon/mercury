# ML Inference Strategy

`InferenceStrategy` runs a trained ONNX model to generate trade signals.

## How it works

1. On each `on_book` call, compute a feature vector from recent order-book snapshots (bid/ask spread, volume imbalance, mid-price returns, etc.).
2. Pass the feature vector through the ONNX model via `tract-onnx`.
3. Interpret the model output as a directional probability; emit buy/sell signals when the probability exceeds a threshold.
4. Fall back to an online logistic regression model when no ONNX file is provided.

## Loading a model

```bash
cargo run --bin mercury -- run \
  --symbol BTCUSDT \
  --paper \
  --strategy inference \
  --model path/to/model.onnx
```

## Online learning

When running without a pre-trained model, `InferenceStrategy` uses incremental logistic regression that updates its weights after each fill. This allows the strategy to adapt to changing market conditions without retraining.

## Feature set

- Mid-price returns (last 10 ticks)
- Bid/ask spread
- Volume imbalance `(bid_qty - ask_qty) / (bid_qty + ask_qty)`
- LOB volume profile (top 5 levels each side)
- Volatility estimate (Parkinson)
