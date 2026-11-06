//! Local inbox and monitoring checkpoints. No UI, Telegram or filesystem types.
use crate::{BookAttachment, CatalogError, DownloadKey};
use std::future::Future;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecommendationStatus {
    #[default]
    Unread,
    Read,
    Hidden,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recommendation {
    pub book: BookAttachment,
    pub source_title: String,
    pub message_date: i64,
    pub status: RecommendationStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredBook {
    pub book: BookAttachment,
    pub message_date: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorPage {
    pub books: Vec<DiscoveredBook>,
    pub scanned: u32,
    pub skipped_protected: u32,
    /// Includes non-book messages, so empty results still advance the watermark.
    pub newest: Option<i32>,
    pub next_offset: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitorCursor {
    pub after: i32,
    /// An unfinished bounded history walk, independent of manual channel browsing.
    pub before: Option<i32>,
    pub target: Option<i32>,
}

impl MonitorCursor {
    pub fn valid(self) -> bool {
        self.after >= 0
            && match (self.before, self.target) {
                (None, None) => true,
                (Some(before), Some(target)) => before > self.after && target >= before,
                _ => false,
            }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecommendationError {
    Storage,
    Unauthorized,
    NoSources,
    Unavailable,
    Telegram(CatalogError),
    Download(crate::DownloadError),
}

impl std::fmt::Display for RecommendationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "recommendation operation failed ({self:?})")
    }
}
impl std::error::Error for RecommendationError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecommendationState {
    pub account: Option<i64>,
    pub loaded: bool,
    pub items: Vec<Recommendation>,
    /// Actual runtime state; durable user intent is stored separately per account.
    pub monitoring: bool,
    pub checking: Option<String>,
    pub error: Option<RecommendationError>,
}

/// Only newly inserted findings, never a replay of the persisted inbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecommendationBatch {
    pub account: i64,
    pub keys: Vec<DownloadKey>,
}

impl RecommendationState {
    pub fn unread_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == RecommendationStatus::Unread)
            .count()
    }
}

pub trait RecommendationStore: Send + Sync {
    fn load_monitoring_enabled(
        &self,
        _account: i64,
    ) -> impl Future<Output = Result<bool, RecommendationError>> + Send {
        async { Ok(false) }
    }
    fn save_monitoring_enabled(
        &self,
        _account: i64,
        _enabled: bool,
    ) -> impl Future<Output = Result<(), RecommendationError>> + Send {
        async { Err(RecommendationError::Storage) }
    }
    fn load_recommendations(
        &self,
        _account: i64,
    ) -> impl Future<Output = Result<Vec<Recommendation>, RecommendationError>> + Send {
        async { Ok(Vec::new()) }
    }
    fn load_monitor_cursor(
        &self,
        _account: i64,
        _channel: i64,
    ) -> impl Future<Output = Result<Option<MonitorCursor>, RecommendationError>> + Send {
        async { Ok(None) }
    }
    /// Findings and their checkpoint commit together, or neither commits.
    fn commit_recommendations(
        &self,
        _account: i64,
        _channel: i64,
        _cursor: MonitorCursor,
        _items: &[Recommendation],
    ) -> impl Future<Output = Result<Vec<DownloadKey>, RecommendationError>> + Send {
        async { Err(RecommendationError::Storage) }
    }
    fn set_recommendation_status(
        &self,
        _account: i64,
        _keys: &[DownloadKey],
        _status: RecommendationStatus,
    ) -> impl Future<Output = Result<(), RecommendationError>> + Send {
        async { Err(RecommendationError::Storage) }
    }
}
