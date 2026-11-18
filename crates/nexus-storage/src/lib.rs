//! Storage roots and every operation that touches user files.
//!
//! All file operations go through [`paths::resolve`], which validates the
//! requested path against an approved storage root before anything on disk
//! is read or written.

pub mod drives;
pub mod health;
mod manager;
pub mod paths;

pub use manager::{AddRootRequest, ConflictPolicy, StorageManager, UploadTarget};
