//! Lightweight keyword parser for news headline sentiment.

use mercury_core::{SentimentSignal, Symbol, now_nanos};

const POSITIVE_KEYWORDS: &[(&str, i32)] = &[
    ("approval", 3),
    ("approves", 3),
    ("approved", 3),
    ("partnership", 2),
    ("beat", 2),
    ("beats", 2),
    ("surge", 2),
    ("surges", 2),
    ("rally", 2),
    ("breakout", 2),
    ("upgrade", 2),
    ("record inflows", 3),
    ("profit", 1),
    ("growth", 1),
    ("bullish", 2),
];

const NEGATIVE_KEYWORDS: &[(&str, i32)] = &[
    ("hack", 3),
    ("hacked", 3),
    ("exploit", 3),
    ("lawsuit", 2),
    ("sec sues", 3),
    ("ban", 3),
    ("banned", 3),
    ("downgrade", 2),
    ("miss", 2),
    ("misses", 2),
    ("plunge", 2),
    ("plunges", 2),
    ("selloff", 2),
    ("bearish", 2),
    ("outflows", 2),
    ("loss", 1),
];

/// Scores news headlines with deterministic keyword matching.
#[derive(Debug, Clone)]
pub struct NewsParser {
    min_abs_score: i32,
}

impl NewsParser {
    pub fn new(min_abs_score: i32) -> Self {
        assert!(min_abs_score > 0, "min_abs_score must be positive");
        Self { min_abs_score }
    }

    pub fn parse(
        &self,
        symbol: Symbol,
        source: impl Into<String>,
        headline: impl Into<String>,
    ) -> Option<SentimentSignal> {
        let headline = headline.into();
        let score = score_headline(&headline);
        if score.abs() < self.min_abs_score {
            return None;
        }

        Some(SentimentSignal {
            symbol,
            score,
            confidence: confidence(score),
            headline,
            source: source.into(),
            timestamp: now_nanos(),
        })
    }
}

impl Default for NewsParser {
    fn default() -> Self {
        Self::new(2)
    }
}

fn score_headline(headline: &str) -> i32 {
    let lower = headline.to_ascii_lowercase();
    let positive: i32 = POSITIVE_KEYWORDS
        .iter()
        .filter_map(|(needle, score)| lower.contains(needle).then_some(*score))
        .sum();
    let negative: i32 = NEGATIVE_KEYWORDS
        .iter()
        .filter_map(|(needle, score)| lower.contains(needle).then_some(*score))
        .sum();
    positive - negative
}

fn confidence(score: i32) -> f32 {
    (score.abs() as f32 / 6.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_headline_emits_signal() {
        let parser = NewsParser::default();
        let signal = parser
            .parse(
                Symbol::new("BTCUSDT"),
                "test",
                "Bitcoin ETF approval sparks bullish rally",
            )
            .unwrap();

        assert!(signal.score > 0);
        assert!(signal.confidence > 0.0);
    }

    #[test]
    fn negative_headline_emits_signal() {
        let parser = NewsParser::default();
        let signal = parser
            .parse(
                Symbol::new("ETHUSDT"),
                "test",
                "Exchange hacked as ETH selloff deepens",
            )
            .unwrap();

        assert!(signal.score < 0);
    }

    #[test]
    fn neutral_headline_is_ignored() {
        let parser = NewsParser::default();
        assert!(
            parser
                .parse(
                    Symbol::new("BTCUSDT"),
                    "test",
                    "Bitcoin traded in a narrow range"
                )
                .is_none()
        );
    }
}
