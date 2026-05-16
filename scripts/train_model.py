#!/usr/bin/env python3
"""
Train a logistic-regression classifier on Mercury feature data and export to ONNX.

Usage:
    pip install -r requirements.txt
    python train_model.py --input labels.csv --output model.onnx

The input CSV must have columns: timestamp, symbol, f0..f9, mid_price, label
where label is -1 (SELL), 0 (HOLD), or 1 (BUY).

The output ONNX model expects input shape [N, 10] (float32) and produces
softmax probabilities of shape [N, 3] for classes [SELL, HOLD, BUY].
"""

import argparse
import sys

import numpy as np
import pandas as pd
from sklearn.linear_model import LogisticRegression
from sklearn.model_selection import train_test_split
from sklearn.pipeline import Pipeline
from sklearn.preprocessing import StandardScaler
from sklearn.metrics import classification_report, confusion_matrix
import skl2onnx
from skl2onnx.common.data_types import FloatTensorType

FEATURES = [f"f{i}" for i in range(10)]
LABEL_MAP = {-1: "SELL", 0: "HOLD", 1: "BUY"}


def load_data(path: str) -> tuple[np.ndarray, np.ndarray]:
    df = pd.read_csv(path)
    missing = [c for c in FEATURES + ["label"] if c not in df.columns]
    if missing:
        sys.exit(f"Error: columns missing from CSV: {missing}")
    X = df[FEATURES].values.astype(np.float32)
    y = df["label"].values.astype(int)
    print(f"Loaded {len(df):,} rows  label distribution: {dict(zip(*np.unique(y, return_counts=True)))}")
    return X, y


def train(X: np.ndarray, y: np.ndarray) -> Pipeline:
    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, shuffle=False  # preserve time order
    )
    pipe = Pipeline([
        ("scaler", StandardScaler()),
        ("clf", LogisticRegression(max_iter=1000, class_weight="balanced")),
    ])
    pipe.fit(X_train, y_train)

    y_pred = pipe.predict(X_test)
    labels = sorted(set(y_test))
    target_names = [LABEL_MAP[l] for l in labels]
    print("\n" + classification_report(y_test, y_pred, labels=labels, target_names=target_names))
    print("Confusion matrix:")
    print(confusion_matrix(y_test, y_pred, labels=labels))
    return pipe


def export_onnx(pipe: Pipeline, output_path: str, n_features: int = 10) -> None:
    initial_type = [("float_input", FloatTensorType([None, n_features]))]
    options = {type(pipe["clf"]): {"zipmap": False}}  # output plain array, not dict
    onnx_model = skl2onnx.convert_sklearn(pipe, initial_types=initial_type, options=options)
    with open(output_path, "wb") as f:
        f.write(onnx_model.SerializeToString())
    print(f"\nSaved ONNX model → {output_path}")
    print("Input:  float_input [N, 10]")
    print("Output: probabilities [N, 3]  (SELL, HOLD, BUY)")


def main() -> None:
    parser = argparse.ArgumentParser(description="Train Mercury ML model")
    parser.add_argument("--input", default="labels.csv", help="Labeled CSV from `mercury dataset`")
    parser.add_argument("--output", default="model.onnx", help="Output ONNX file")
    args = parser.parse_args()

    X, y = load_data(args.input)
    pipe = train(X, y)
    export_onnx(pipe, args.output)


if __name__ == "__main__":
    main()
