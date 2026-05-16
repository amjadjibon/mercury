//! Dataset generation from recorded Parquet tick data.
//!
//! Replays a Parquet file, computes features at every `BookUpdate`, looks
//! ahead `N` ticks to assign a directional label, and writes a CSV file
//! ready for Python training.

use anyhow::Result;
use mercury_core::{EventPayload, OrderBook};
use mercury_strategy::{FEATURE_COUNT, FeatureComputer};
use std::collections::VecDeque;
use std::path::PathBuf;

/// Label convention:
/// - `1`  (BUY)  if `mid[t+N] > mid[t] × (1 + threshold)`
/// - `-1` (SELL) if `mid[t+N] < mid[t] × (1 − threshold)`
/// - `0`  (HOLD) otherwise
pub struct DatasetGenerator {
    lookahead: usize,
    threshold: f64,
    features: FeatureComputer,
    /// Ring buffer: (mid_price_f64, feature_row, timestamp_ns, symbol)
    window: VecDeque<(f64, [f32; FEATURE_COUNT], i64, String)>,
    writer: csv::Writer<std::fs::File>,
    rows_written: u64,
}

impl DatasetGenerator {
    /// Create a new generator.
    ///
    /// - `output` — path to write the CSV file.
    /// - `lookahead` — number of `BookUpdate` ticks to look ahead for labelling.
    /// - `threshold` — fractional price move threshold for BUY/SELL labels (e.g. 0.0005 = 5 bps).
    pub fn new(output: PathBuf, lookahead: usize, threshold: f64) -> Result<Self> {
        let file = std::fs::File::create(&output)?;
        let mut writer = csv::Writer::from_writer(file);

        // Write header
        let mut header: Vec<String> = vec!["timestamp".into(), "symbol".into()];
        for i in 0..FEATURE_COUNT {
            header.push(format!("f{}", i));
        }
        header.push("mid_price".into());
        header.push("label".into());
        writer.write_record(&header)?;

        Ok(Self {
            lookahead,
            threshold,
            features: FeatureComputer::new(),
            window: VecDeque::with_capacity(lookahead + 1),
            writer,
            rows_written: 0,
        })
    }

    /// Process a single event from the replay stream.
    pub fn on_event(
        &mut self,
        event: &mercury_core::Event,
        books: &std::collections::HashMap<mercury_core::Symbol, OrderBook>,
    ) -> Result<()> {
        match &event.payload {
            EventPayload::BookUpdate(upd) => {
                let sym = upd.symbol;
                if let Some(book) = books.get(&sym) {
                    let mid = match book.mid_price() {
                        Some(m) => m,
                        None => return Ok(()),
                    };
                    let mid_f64: f64 = mid.to_string().parse().unwrap_or(0.0);
                    if let Some(feats) = self.features.compute(book) {
                        self.window.push_back((
                            mid_f64,
                            feats,
                            event.timestamp,
                            sym.as_str().to_string(),
                        ));
                    }
                    // Label the row that is `lookahead` steps behind
                    if self.window.len() > self.lookahead {
                        let (old_mid, old_feats, old_ts, old_sym) =
                            self.window.pop_front().unwrap();
                        let label = Self::label(old_mid, mid_f64, self.threshold);
                        self.write_row(old_ts, &old_sym, &old_feats, old_mid, label)?;
                    }
                }
            }
            EventPayload::Trade(trade) => {
                self.features.on_trade(trade);
            }
            _ => {}
        }
        Ok(())
    }

    fn label(past_mid: f64, future_mid: f64, threshold: f64) -> i8 {
        if future_mid > past_mid * (1.0 + threshold) {
            1
        } else if future_mid < past_mid * (1.0 - threshold) {
            -1
        } else {
            0
        }
    }

    fn write_row(
        &mut self,
        ts: i64,
        sym: &str,
        feats: &[f32; FEATURE_COUNT],
        mid: f64,
        label: i8,
    ) -> Result<()> {
        let mut row: Vec<String> = vec![ts.to_string(), sym.to_string()];
        for &f in feats.iter() {
            row.push(format!("{:.8}", f));
        }
        row.push(format!("{:.8}", mid));
        row.push(label.to_string());
        self.writer.write_record(&row)?;
        self.rows_written += 1;
        Ok(())
    }

    /// Flush remaining buffered rows (HOLD-labelled — no future mid available).
    pub fn close(mut self) -> Result<u64> {
        // Drain window — these rows have no lookahead; label as HOLD (0)
        while let Some((old_mid, old_feats, old_ts, old_sym)) = self.window.pop_front() {
            self.write_row(old_ts, &old_sym, &old_feats, old_mid, 0)?;
        }
        self.writer.flush()?;
        Ok(self.rows_written)
    }
}

/// Convenience: generate a dataset by replaying a Parquet file.
pub async fn generate_dataset(
    parquet_path: PathBuf,
    output: PathBuf,
    lookahead: usize,
    threshold: f64,
) -> Result<u64> {
    use crate::Player;
    use mercury_core::EventBus;
    use std::sync::Arc;

    let event_bus = Arc::new(EventBus::new(100_000));
    let mut rx = event_bus.subscribe();

    // Replay all events into the bus at max speed
    Player::new(parquet_path, 0.0)
        .play(Arc::clone(&event_bus))
        .await?;
    event_bus.close();

    let mut generator = DatasetGenerator::new(output, lookahead, threshold)?;
    let mut books: std::collections::HashMap<mercury_core::Symbol, OrderBook> =
        std::collections::HashMap::new();

    loop {
        match rx.try_recv() {
            Ok(event) => {
                // Maintain a live order book per symbol for feature computation
                if let EventPayload::BookUpdate(ref upd) = event.payload {
                    let book = books
                        .entry(upd.symbol)
                        .or_insert_with(|| OrderBook::new(upd.exchange, upd.symbol));
                    book.apply_update(upd);
                }
                generator.on_event(&event, &books)?;
            }
            Err(mercury_core::RecvError::Empty) | Err(mercury_core::RecvError::Closed) => break,
            Err(mercury_core::RecvError::Lagged(_)) => {}
        }
    }

    generator.close()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_label_assignment() {
        let past = 50000.0f64;
        // BUY: future > past * 1.0005
        assert_eq!(DatasetGenerator::label(past, past * 1.001, 0.0005), 1);
        // SELL: future < past * 0.9995
        assert_eq!(DatasetGenerator::label(past, past * 0.999, 0.0005), -1);
        // HOLD: within band
        assert_eq!(DatasetGenerator::label(past, past * 1.0001, 0.0005), 0);
    }

    #[test]
    fn test_csv_header_written() -> Result<()> {
        let tmp = NamedTempFile::new()?;
        let path = tmp.path().to_path_buf();
        let generator = DatasetGenerator::new(path.clone(), 10, 0.0005)?;
        generator.close()?;

        let content = std::fs::read_to_string(&path)?;
        assert!(content.starts_with("timestamp,symbol,f0,f1"));
        assert!(content.contains("mid_price,label"));
        Ok(())
    }
}
