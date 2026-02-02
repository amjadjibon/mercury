//! Mercury Core - Shared types, event bus, and memory primitives
//!
//! This crate provides the foundation for the Mercury trading engine:
//! - Common types (Symbol, Exchange, Side, etc.)
//! - Event types and event bus for inter-component communication
//! - Order book data structure
//! - Memory pool for zero-allocation hot paths

pub mod event_bus;
pub mod events;
pub mod orderbook;
pub mod pool;
pub mod types;

pub use event_bus::EventBus;
pub use events::*;
pub use orderbook::OrderBook;
pub use pool::Pool;
pub use types::*;
