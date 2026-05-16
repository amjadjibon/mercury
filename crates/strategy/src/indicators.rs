//! Technical Analysis indicators.

use rust_decimal::Decimal;
use std::collections::VecDeque;

/// Trait for rolling window indicators.
pub trait Window {
    /// Add a new value to the window.
    fn update(&mut self, value: Decimal) -> Option<Decimal>;
    /// Get the current value of the indicator.
    fn value(&self) -> Option<Decimal>;
    /// Reset the indicator state.
    fn reset(&mut self);
}

/// Simple Moving Average (SMA).
#[derive(Debug, Clone)]
pub struct Sma {
    window_size: usize,
    values: VecDeque<Decimal>,
    sum: Decimal,
}

impl Sma {
    /// Create a new SMA indicator.
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size,
            values: VecDeque::with_capacity(window_size),
            sum: Decimal::ZERO,
        }
    }
}

impl Window for Sma {
    fn update(&mut self, value: Decimal) -> Option<Decimal> {
        self.values.push_back(value);
        self.sum += value;

        if self.values.len() > self.window_size {
            if let Some(removed) = self.values.pop_front() {
                self.sum -= removed;
            }
        }

        self.value()
    }

    fn value(&self) -> Option<Decimal> {
        if self.values.len() < self.window_size {
            None
        } else {
            Some(self.sum / Decimal::from(self.window_size))
        }
    }

    fn reset(&mut self) {
        self.values.clear();
        self.sum = Decimal::ZERO;
    }
}

/// Exponential Moving Average (EMA).
#[derive(Debug, Clone)]
pub struct Ema {
    #[allow(dead_code)]
    window_size: usize,
    k: Decimal,
    current_ema: Option<Decimal>,
}

impl Ema {
    /// Create a new EMA indicator.
    pub fn new(window_size: usize) -> Self {
        // k = 2 / (N + 1)
        let k = Decimal::from(2) / Decimal::from(window_size + 1);
        Self {
            window_size,
            k,
            current_ema: None,
        }
    }
}

impl Window for Ema {
    fn update(&mut self, value: Decimal) -> Option<Decimal> {
        match self.current_ema {
            Some(prev_ema) => {
                // EMA_today = (Value_today * (k)) + (EMA_yesterday * (1 - k))
                let new_ema = (value * self.k) + (prev_ema * (Decimal::ONE - self.k));
                self.current_ema = Some(new_ema);
            }
            None => {
                // Initialize with first value (or could use SMA of first N values)
                self.current_ema = Some(value);
            }
        }
        self.current_ema
    }

    fn value(&self) -> Option<Decimal> {
        self.current_ema
    }

    fn reset(&mut self) {
        self.current_ema = None;
    }
}

/// Relative Strength Index (RSI).
#[derive(Debug, Clone)]
pub struct Rsi {
    #[allow(dead_code)]
    window_size: usize,
    prev_value: Option<Decimal>,
    gains: Ema,
    losses: Ema,
}

impl Rsi {
    /// Create a new RSI indicator.
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size,
            prev_value: None,
            // Wilder's Smoothing is equivalent to EMA with alpha = 1 / N
            // Which is approximately EMA with window = 2 * N - 1
            gains: Ema::new(window_size * 2 - 1),
            losses: Ema::new(window_size * 2 - 1),
        }
    }
}

impl Window for Rsi {
    fn update(&mut self, value: Decimal) -> Option<Decimal> {
        if let Some(prev) = self.prev_value {
            let change = value - prev;
            let gain = if change > Decimal::ZERO {
                change
            } else {
                Decimal::ZERO
            };
            let loss = if change < Decimal::ZERO {
                -change
            } else {
                Decimal::ZERO
            };

            self.gains.update(gain);
            self.losses.update(loss);

            self.prev_value = Some(value);

            self.value()
        } else {
            self.prev_value = Some(value);
            None
        }
    }

    fn value(&self) -> Option<Decimal> {
        let avg_gain = self.gains.value().unwrap_or_default();
        let avg_loss = self.losses.value().unwrap_or_default();

        if avg_loss == Decimal::ZERO {
            if avg_gain == Decimal::ZERO {
                return None; // Not enough data
            }
            return Some(Decimal::from(100));
        }

        let rs = avg_gain / avg_loss;
        let rsi = Decimal::from(100) - (Decimal::from(100) / (Decimal::ONE + rs));
        Some(rsi)
    }

    fn reset(&mut self) {
        self.prev_value = None;
        self.gains.reset();
        self.losses.reset();
    }
}

/// Moving Average Convergence Divergence (MACD).
#[derive(Debug, Clone)]
pub struct Macd {
    fast_ema: Ema,
    slow_ema: Ema,
    signal_ema: Ema,
    macd_line: Option<Decimal>,
}

impl Macd {
    /// Create a new MACD indicator (standard is 12, 26, 9).
    pub fn new(fast_period: usize, slow_period: usize, signal_period: usize) -> Self {
        Self {
            fast_ema: Ema::new(fast_period),
            slow_ema: Ema::new(slow_period),
            signal_ema: Ema::new(signal_period),
            macd_line: None,
        }
    }

    /// Update with new value and return (macd_line, signal_line, histogram).
    pub fn update_full(&mut self, value: Decimal) -> Option<(Decimal, Decimal, Decimal)> {
        let fast = self.fast_ema.update(value)?;
        let slow = self.slow_ema.update(value)?;

        let macd_line = fast - slow;
        self.macd_line = Some(macd_line);

        let signal_line = self.signal_ema.update(macd_line)?;
        let histogram = macd_line - signal_line;

        Some((macd_line, signal_line, histogram))
    }
}

impl Window for Macd {
    fn update(&mut self, value: Decimal) -> Option<Decimal> {
        self.update_full(value).map(|(_, _, hist)| hist)
    }

    fn value(&self) -> Option<Decimal> {
        // Return histogram by default as it's the primary signal
        if let (Some(macd), Some(signal)) = (self.macd_line, self.signal_ema.value()) {
            Some(macd - signal)
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.fast_ema.reset();
        self.slow_ema.reset();
        self.signal_ema.reset();
        self.macd_line = None;
    }
}

/// Exponential ATR (Average True Range proxy using bid/ask spread as range).
///
/// Uses EMA smoothing on the spread value, normalised by caller (÷ mid_price).
#[derive(Debug, Clone)]
pub struct Atr {
    ema: Ema,
}

impl Atr {
    pub fn new(window_size: usize) -> Self {
        Self { ema: Ema::new(window_size) }
    }
}

impl Window for Atr {
    fn update(&mut self, spread: Decimal) -> Option<Decimal> {
        self.ema.update(spread)
    }

    fn value(&self) -> Option<Decimal> {
        self.ema.value()
    }

    fn reset(&mut self) {
        self.ema.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_sma() {
        let mut sma = Sma::new(3);
        assert_eq!(sma.update(dec!(10)), None);
        assert_eq!(sma.update(dec!(20)), None);
        assert_eq!(sma.update(dec!(30)), Some(dec!(20))); // (10+20+30)/3 = 20
        assert_eq!(sma.update(dec!(40)), Some(dec!(30))); // (20+30+40)/3 = 30
    }

    #[test]
    fn test_ema() {
        let mut ema = Ema::new(3); // k = 0.5
        assert_eq!(ema.update(dec!(10)), Some(dec!(10)));
        assert_eq!(ema.update(dec!(20)), Some(dec!(15))); // 10*0.5 + 20*0.5 = 15
        assert_eq!(ema.update(dec!(30)), Some(dec!(22.5))); // 15*0.5 + 30*0.5 = 22.5
    }

    #[test]
    fn test_rsi() {
        let mut rsi = Rsi::new(14);
        // Feed increasing prices
        for i in 0..20 {
            rsi.update(Decimal::from(i * 10));
        }
        let val = rsi.value().unwrap();
        assert!(val > dec!(70)); // Should be overbought
    }
}
