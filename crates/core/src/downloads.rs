//! Transfer ports contain no Telegram, filesystem or UI implementation.

use std::{future::Future, path::PathBuf, pin::Pin};

use async_channel::Sender;
use tokio_util::sync::CancellationToken;

use crate::{BookAttachment, DownloadKey, DownloadRecord, TransferMetrics};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadError {
    Busy,
    QueueFull,
    Unauthorized,
    Unavailable,
    ProtectedContent,
    FloodWait(u32),
    Network,
    SecureStorage,
    Storage,
    Destination,
    FileExists,
    AlreadyDownloaded,
    InvalidFile,
    Incomplete,
    Cancelled,
    UnsupportedReply,
    Internal,
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "download failed ({self:?})")
    }
}

impl std::error::Error for DownloadError {}

pub type ChunkFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, DownloadError>> + Send + 'a>>;

/// Bounded chunks, never the complete file in memory.
pub trait BookChunks: Send {
    fn next_chunk(&mut self) -> ChunkFuture<'_>;
}

pub struct RemoteBook {
    /// Fresh metadata, verified against the requested Telegram identity.
    pub book: BookAttachment,
    pub chunks: Box<dyn BookChunks>,
}

pub type BookFuture =
    Pin<Box<dyn Future<Output = Result<RemoteBook, DownloadError>> + Send + 'static>>;

pub trait BookSource: Send {
    /// The owned future lets transfer work run without borrowing the auth actor.
    fn open_book(&self, _key: DownloadKey) -> BookFuture {
        Box::pin(async { Err(DownloadError::Unavailable) })
    }
}

/// Atomic admission and durable transitions are supplied by the storage adapter.
pub trait DownloadRepository: Clone + Send + Sync + 'static {
    fn claim(
        &self,
        record: &DownloadRecord,
    ) -> impl Future<Output = Result<bool, DownloadError>> + Send;
    fn update(
        &self,
        record: &DownloadRecord,
    ) -> impl Future<Output = Result<(), DownloadError>> + Send;
    fn recover_interrupted(&self) -> impl Future<Output = Result<(), DownloadError>> + Send;
}

pub trait DownloadEngine: Clone + Send + Sync + 'static {
    fn recover(&self) -> impl Future<Output = Result<(), DownloadError>> + Send;
    fn save(
        &self,
        remote: RemoteBook,
        directory: PathBuf,
        updates: Sender<DownloadUpdate>,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<DownloadRecord, DownloadError>> + Send;
}

/// Live telemetry is deliberately separate from durable download records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadUpdate {
    Record(DownloadRecord),
    Metrics {
        key: DownloadKey,
        metrics: TransferMetrics,
    },
}

impl From<DownloadRecord> for DownloadUpdate {
    fn from(record: DownloadRecord) -> Self {
        Self::Record(record)
    }
}

/// Waiting jobs retain their metadata even after the user changes channels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedDownload {
    pub book: BookAttachment,
    pub directory: PathBuf,
}

pub const MAX_PENDING_DOWNLOADS: usize = 256;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TransferState {
    pub active: Option<DownloadKey>,
    pub cancelling: bool,
    pub error: Option<DownloadError>,
    pub error_file: Option<String>,
    pub queued: Vec<QueuedDownload>,
    /// Pausing prevents the next start; it does not interrupt the active file.
    pub paused: bool,
    pub skipped_duplicates: usize,
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct UnusedDownloads;

#[cfg(test)]
impl DownloadEngine for UnusedDownloads {
    async fn recover(&self) -> Result<(), DownloadError> {
        Ok(())
    }
    async fn save(
        &self,
        _: RemoteBook,
        _: PathBuf,
        _: Sender<DownloadUpdate>,
        _: CancellationToken,
    ) -> Result<DownloadRecord, DownloadError> {
        Err(DownloadError::Unavailable)
    }
}
