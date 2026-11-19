//! Shared types, errors and utilities used across the NEXUS crates.

pub mod error;
pub mod events;
pub mod kinds;
pub mod runtime;
pub mod time;
pub mod types;

pub use error::{NexusError, Result};

/// Application version, shared by the service and the desktop shell.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Generates a new time-ordered unique identifier (UUID v7).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}
