//! Platform-independent models and the typed application service.
//!
//! Adapters depend on this crate, never the other way around.

mod auth;
mod catalog;
mod downloads;
mod models;
mod recommendations;
mod service;
mod settings;
mod sources;
mod transfer_metrics;

pub use auth::*;
pub use catalog::*;
pub use downloads::*;
pub use models::*;
pub use recommendations::*;
pub use service::*;
pub use settings::*;
pub use sources::*;
pub use transfer_metrics::*;
