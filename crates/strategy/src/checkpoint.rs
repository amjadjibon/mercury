//! Model weight persistence for online classifiers and the DQN Q-network.
//!
//! **`OnlineClassifier`** — serialized as compact JSON (human-readable, small).
//! **DQN (`RlQuotePlacementStrategy`)** — serialized as raw `f32` little-endian
//! bytes (fast, compact, no parse overhead).
//!
//! Each `Strategy` calls `save` on `reset()` and loads weights in `new()` when
//! a checkpoint path is provided.

use crate::features::FEATURE_COUNT;
use crate::inference_strategy::OnlineClassifier;
use std::io::{self, Read, Write};

// ── OnlineClassifier checkpoint ───────────────────────────────────────────────

/// Flat representation used for JSON serialization.
#[derive(Debug)]
struct ClassifierState {
    weights: Vec<f32>,    // 3 × FEATURE_COUNT, row-major
    bias: Vec<f32>,       // 3
    updates: u64,
}

impl OnlineClassifier {
    /// Save weights + bias to a JSON file at `path`.
    pub fn save(&self, path: &str) -> io::Result<()> {
        let weights_flat: Vec<f32> = self.weights().iter().flat_map(|row| row.iter().copied()).collect();
        let bias_vec: Vec<f32> = self.bias().to_vec();
        let updates = self.updates();

        let json = format!(
            r#"{{"updates":{},"bias":[{}],"weights":[{}]}}"#,
            updates,
            bias_vec.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","),
            weights_flat.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","),
        );

        let mut file = std::fs::File::create(path)?;
        file.write_all(json.as_bytes())?;
        Ok(())
    }

    /// Load weights + bias from a JSON file.  Overwrites current state.
    pub fn load(&mut self, path: &str) -> io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;

        let state = parse_classifier_json(&buf).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e)
        })?;

        if state.weights.len() != 3 * FEATURE_COUNT || state.bias.len() != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "checkpoint dimension mismatch: got {}w {}b, expected {}w 3b",
                    state.weights.len(),
                    state.bias.len(),
                    3 * FEATURE_COUNT
                ),
            ));
        }

        for class in 0..3 {
            for j in 0..FEATURE_COUNT {
                self.weights_mut()[class][j] = state.weights[class * FEATURE_COUNT + j];
            }
            self.bias_mut()[class] = state.bias[class];
        }
        *self.updates_mut() = state.updates;
        Ok(())
    }
}

/// Minimal hand-rolled JSON parser for the checkpoint format.
fn parse_classifier_json(s: &str) -> Result<ClassifierState, String> {
    let updates = extract_u64(s, "updates")?;
    let bias = extract_f32_array(s, "bias")?;
    let weights = extract_f32_array(s, "weights")?;
    Ok(ClassifierState { weights, bias, updates })
}

fn extract_u64(s: &str, key: &str) -> Result<u64, String> {
    let pattern = format!("\"{}\":", key);
    let start = s.find(&pattern).ok_or_else(|| format!("key '{}' not found", key))? + pattern.len();
    let rest = s[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse::<u64>().map_err(|e| e.to_string())
}

fn extract_f32_array(s: &str, key: &str) -> Result<Vec<f32>, String> {
    let pattern = format!("\"{}\":[", key);
    let start = s.find(&pattern).ok_or_else(|| format!("key '{}' not found", key))? + pattern.len();
    let end = s[start..].find(']').ok_or("missing ']'")?;
    let inner = &s[start..start + end];
    inner
        .split(',')
        .filter(|t| !t.trim().is_empty())
        .map(|t| t.trim().parse::<f32>().map_err(|e| e.to_string()))
        .collect()
}

// ── DQN network checkpoint ────────────────────────────────────────────────────

/// Flat f32 LE byte blob for a weight matrix / bias vector.
fn write_f32_slice(buf: &mut Vec<u8>, v: &[f32]) {
    for &x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
}

fn read_f32_slice(cursor: &mut usize, raw: &[u8], n: usize) -> io::Result<Vec<f32>> {
    let byte_len = n * 4;
    if *cursor + byte_len > raw.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "DQN checkpoint truncated"));
    }
    let out: Vec<f32> = raw[*cursor..*cursor + byte_len]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    *cursor += byte_len;
    Ok(out)
}

/// Save a flat `Vec<f32>` weight blob to a binary file.
pub fn save_f32_weights(path: &str, layers: &[&[f32]]) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    for layer in layers {
        write_f32_slice(&mut buf, layer);
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(&buf)?;
    Ok(())
}

/// Load and verify a flat f32 weight blob.
pub fn load_f32_weights(path: &str, expected_total: usize) -> io::Result<Vec<f32>> {
    let mut file = std::fs::File::open(path)?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    if raw.len() != expected_total * 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "DQN checkpoint size mismatch: got {} bytes, expected {}",
                raw.len(),
                expected_total * 4
            ),
        ));
    }
    let mut cursor = 0;
    read_f32_slice(&mut cursor, &raw, expected_total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_strategy::OnlineClassifierConfig;

    fn tmp_path(name: &str) -> String {
        format!("/tmp/mercury_test_{name}.json")
    }

    #[test]
    fn test_classifier_round_trip() {
        let config = OnlineClassifierConfig::default();
        let mut clf = OnlineClassifier::new(config);

        // Do a few updates so weights are non-zero.
        use crate::features::FEATURE_COUNT;
        let feats = [0.1f32; FEATURE_COUNT];
        clf.update(feats, 1);
        clf.update(feats, -1);

        let path = tmp_path("clf_rt");
        clf.save(&path).expect("save failed");

        let mut clf2 = OnlineClassifier::new(config);
        clf2.load(&path).expect("load failed");

        let p1 = clf.predict(feats);
        let p2 = clf2.predict(feats);
        for i in 0..3 {
            assert!(
                (p1[i] - p2[i]).abs() < 1e-5,
                "probability mismatch at class {i}: {p1:?} vs {p2:?}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_wrong_dimensions_returns_error() {
        let path = tmp_path("clf_bad");
        // Write a malformed checkpoint.
        std::fs::write(&path, r#"{"updates":0,"bias":[0],"weights":[0]}"#).unwrap();
        let mut clf = OnlineClassifier::new(OnlineClassifierConfig::default());
        assert!(clf.load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
