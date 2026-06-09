//! Core types for the Mercury trading engine.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Price type using Decimal for precise financial calculations.
pub type Price = Decimal;

/// Quantity type using Decimal for precise amounts.
pub type Quantity = Decimal;

/// Scale factor for `FixedPoint`: 1.0 == 100_000_000 (10^8 implicit decimal places).
pub const FP_SCALE: i64 = 100_000_000;

/// Stack-allocated fixed-point value with 8 implicit decimal places.
///
/// Used for `Level.price` and `Level.quantity` on the hot path. Integer arithmetic
/// is ~30× faster than `rust_decimal::Decimal` and avoids all heap allocation.
///
/// Representation: `raw / FP_SCALE`. Example: BTC at $50 000 → `5_000_000_000_000i64`.
///
/// Conversions to/from `Decimal` are provided for interop with non-hot-path code.
/// Use `FixedPoint::from_str()` in parsers to skip the `Decimal` detour entirely.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct FixedPoint(pub i64);

impl FixedPoint {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(FP_SCALE);

    #[inline(always)]
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    #[inline(always)]
    pub fn to_decimal(self) -> Decimal {
        Decimal::from(self.0) / Decimal::from(FP_SCALE)
    }

    /// Convert from `Decimal`. Only used in non-hot-path initialization; goes via string.
    pub fn from_decimal(d: Decimal) -> Self {
        Self::from_str_decimal(&d.to_string()).unwrap_or(Self::ZERO)
    }

    #[inline(always)]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / FP_SCALE as f64
    }

    #[inline(always)]
    pub fn from_f64(f: f64) -> Self {
        Self((f * FP_SCALE as f64).round() as i64)
    }

    /// Parse a decimal-format byte string directly into fixed-point — no `Decimal` allocation.
    ///
    /// Accepts formats like `"50000"`, `"50000.12"`, `"0.00010000"`, `"-1.5"`.
    /// Returns `None` if the string is not a valid decimal number.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Self::from_str_decimal(s)
    }

    fn from_str_decimal(s: &str) -> Option<Self> {
        let s = s.trim();
        let (neg, s) = if let Some(rest) = s.strip_prefix('-') {
            (true, rest)
        } else {
            (false, s)
        };

        let raw = if let Some(dot) = s.find('.') {
            let int_part: i64 = if s[..dot].is_empty() {
                0
            } else {
                s[..dot].parse().ok()?
            };
            let frac_str = &s[dot + 1..];
            // Cap to 8 digits: more precision is discarded, and longer strings can overflow i64.
            let frac_len_orig = frac_str.len();
            let frac_str = if frac_len_orig > 8 { &frac_str[..8] } else { frac_str };
            let frac_len = frac_str.len();
            let frac: i64 = if frac_str.is_empty() {
                0
            } else {
                frac_str.parse().ok()?
            };
            let scaled_frac = if frac_len >= 8 {
                frac // already 8 digits, no scaling needed
            } else {
                frac * 10i64.pow((8 - frac_len) as u32)
            };
            int_part * FP_SCALE + scaled_frac
        } else {
            s.parse::<i64>().ok()? * FP_SCALE
        };

        Some(Self(if neg { -raw } else { raw }))
    }
}

impl From<Decimal> for FixedPoint {
    fn from(d: Decimal) -> Self {
        Self::from_decimal(d)
    }
}

impl From<FixedPoint> for Decimal {
    fn from(fp: FixedPoint) -> Self {
        fp.to_decimal()
    }
}

impl From<i64> for FixedPoint {
    fn from(n: i64) -> Self {
        Self(n * FP_SCALE)
    }
}

/// Allows `assert_eq!(level.price, dec!(50000))` in tests without converting call sites.
impl PartialEq<Decimal> for FixedPoint {
    fn eq(&self, other: &Decimal) -> bool {
        self.to_decimal() == *other
    }
}

impl PartialOrd<Decimal> for FixedPoint {
    fn partial_cmp(&self, other: &Decimal) -> Option<std::cmp::Ordering> {
        self.to_decimal().partial_cmp(other)
    }
}

impl std::ops::Add for FixedPoint {
    type Output = Self;
    #[inline(always)]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for FixedPoint {
    type Output = Self;
    #[inline(always)]
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl std::ops::Neg for FixedPoint {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl std::ops::Mul for FixedPoint {
    type Output = Self;
    #[inline(always)]
    fn mul(self, rhs: Self) -> Self {
        // i128 intermediate prevents overflow: BTC@50k → raw 5e12; 5e12*5e12 = 2.5e25 > i64::MAX.
        Self(((self.0 as i128 * rhs.0 as i128) / FP_SCALE as i128) as i64)
    }
}

impl std::ops::Div for FixedPoint {
    type Output = Self;
    #[inline(always)]
    fn div(self, rhs: Self) -> Self {
        Self(((self.0 as i128 * FP_SCALE as i128) / rhs.0 as i128) as i64)
    }
}

impl std::iter::Sum for FixedPoint {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |acc, x| Self(acc.0 + x.0))
    }
}

impl fmt::Debug for FixedPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.8}", self.to_f64())
    }
}

impl fmt::Display for FixedPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_decimal())
    }
}

impl Serialize for FixedPoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.to_f64())
    }
}

impl<'de> Deserialize<'de> for FixedPoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = FixedPoint;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a number or numeric string")
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<FixedPoint, E> {
                Ok(FixedPoint::from_f64(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<FixedPoint, E> {
                Ok(FixedPoint(v * FP_SCALE))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<FixedPoint, E> {
                Ok(FixedPoint(v as i64 * FP_SCALE))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<FixedPoint, E> {
                FixedPoint::from_str(v)
                    .ok_or_else(|| E::custom("invalid fixed-point string"))
            }
        }
        d.deserialize_any(Visitor)
    }
}

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
    Polymarket,
    Kalshi,
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
            Exchange::Polymarket => write!(f, "Polymarket"),
            Exchange::Kalshi => write!(f, "Kalshi"),
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
#[derive(Default)]
pub enum TimeInForce {
    /// Good till cancelled
    #[default]
    GTC,
    /// Immediate or cancel
    IOC,
    /// Fill or kill
    FOK,
    /// Rest only; reject if it would immediately match.
    PostOnly,
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
