//! Core types for the Mercury trading engine.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Price type using Decimal for precise financial calculations.
pub type Price = Decimal;

/// Quantity type using Decimal for precise amounts.
pub type Quantity = Decimal;

/// Unique order identifier.
pub type OrderId = u64;

/// Timestamp in nanoseconds since Unix epoch.
pub type Timestamp = i64;

/// Trading symbol (e.g., BTCUSDT).
/// Fixed-size array to avoid allocations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Symbol {
    data: [u8; 16],
    len: u8,
}

impl Symbol {
    /// Create a new symbol from a string slice.
    pub fn new(s: &str) -> Self {
        let mut data = [0u8; 16];
        let len = s.len().min(16);
        data[..len].copy_from_slice(&s.as_bytes()[..len]);
        Self {
            data,
            len: len as u8,
        }
    }

    /// Get the symbol as a string slice.
    pub fn as_str(&self) -> &str {
        // SAFETY: We only store valid UTF-8 from the constructor
        unsafe { std::str::from_utf8_unchecked(&self.data[..self.len as usize]) }
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Symbol({})", self.as_str())
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Supported exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exchange {
    Binance,
    Bybit,
    Coinbase,
    Kraken,
    Okx,
    Yahoo,
}

impl fmt::Display for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exchange::Binance => write!(f, "Binance"),
            Exchange::Bybit => write!(f, "Bybit"),
            Exchange::Coinbase => write!(f, "Coinbase"),
            Exchange::Kraken => write!(f, "Kraken"),
            Exchange::Okx => write!(f, "OKX"),
            Exchange::Yahoo => write!(f, "Yahoo"),
        }
    }
}

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    /// Returns the opposite side.
    pub fn opposite(&self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
    Limit,
    Market,
}

/// Order status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// Time in force for orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good till cancelled
    GTC,
    /// Immediate or cancel
    IOC,
    /// Fill or kill
    FOK,
}

impl Default for TimeInForce {
    fn default() -> Self {
        Self::GTC
    }
}

/// Get current timestamp in nanoseconds.
pub fn now_nanos() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("Time went backwards")
        .as_nanos() as Timestamp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol() {
        let sym = Symbol::new("BTCUSDT");
        assert_eq!(sym.as_str(), "BTCUSDT");
        assert_eq!(format!("{}", sym), "BTCUSDT");
    }

    #[test]
    fn test_symbol_truncation() {
        let long = "VERYLONGSYMBOLNAME";
        let sym = Symbol::new(long);
        assert_eq!(sym.as_str(), "VERYLONGSYMBOLNA");
    }

    #[test]
    fn test_side_opposite() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Sell.opposite(), Side::Buy);
    }
}
