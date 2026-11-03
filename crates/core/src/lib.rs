//! Platform-independent models and the typed application service.
//!
//! Adapters depend on this crate, never the other way around.

mod models;
mod service;

pub use models::*;
pub use service::*;
