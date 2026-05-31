//! Mercury Core - Shared types, event bus, and memory primitives
//!
//! This crate provides the foundation for the Mercury trading engine:
//! - Common types (Symbol, Exchange, Side, etc.)
//! - Event types and event bus for inter-component communication
//! - Order book data structure
//! - Memory pool for zero-allocation hot paths

pub mod event_bus;
pub mod events;
pub mod ipc;
pub mod orderbook;
pub mod pool;
pub mod ring_buffer;
pub mod shmem;
pub mod types;

pub use event_bus::{EventBus, PublishError};
pub use ring_buffer::{RecvError, Subscriber};
pub use events::*;
pub use ipc::IpcServer;
pub use orderbook::OrderBook;
pub use pool::Pool;
pub use types::*;
