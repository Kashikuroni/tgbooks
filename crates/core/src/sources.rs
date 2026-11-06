//! Saved source intent is account-scoped and independent of navigation or scanning.
use std::future::Future;

use crate::ChannelSummary;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceError {
    Unauthorized,
    Unavailable,
    Storage,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedSources {
    pub account: Option<i64>,
    pub channels: Vec<ChannelSummary>,
    pub loaded: bool,
    pub error: Option<SourceError>,
}

impl SavedSources {
    pub fn contains(&self, id: i64) -> bool {
        self.channels.iter().any(|channel| channel.id == id)
    }
}

pub trait SourceStore: Send + Sync {
    fn load_sources(
        &self,
        _account: i64,
    ) -> impl Future<Output = Result<Vec<ChannelSummary>, SourceError>> + Send {
        async { Ok(Vec::new()) }
    }

    /// Atomic row-level change. Failure must leave the previous intent intact.
    fn set_source(
        &self,
        _account: i64,
        _channel: &ChannelSummary,
        _enabled: bool,
    ) -> impl Future<Output = Result<(), SourceError>> + Send {
        async { Err(SourceError::Storage) }
    }
}
