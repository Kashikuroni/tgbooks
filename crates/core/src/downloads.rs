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

impl DownloadError {
    /// Keep durable intent and a verified prefix; retry only after user consent.
    pub fn is_resumable(self) -> bool {
        matches!(
            self,
            Self::Network
                | Self::FloodWait(_)
                | Self::Storage
                | Self::Destination
                | Self::Internal
                | Self::SecureStorage
        )
    }
}

pub type ChunkFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, DownloadError>> + Send + 'a>>;

/// Bounded chunks, never the complete file in memory.
pub trait BookChunks: Send {
    fn next_chunk(&mut self) -> ChunkFuture<'_>;
    /// Reposition before the first read. Implementations must return bytes at
    /// exactly this offset, even when their transport requires aligned requests.
    fn seek(&mut self, offset: u64) -> Result<(), DownloadError> {
        if offset == 0 {
            Ok(())
        } else {
            Err(DownloadError::UnsupportedReply)
        }
    }
}

/// A committed prefix, never an assumption based on a file's length alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadCheckpoint {
    pub part_path: PathBuf,
    pub file_name: String,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub sha256: [u8; 32],
    /// EOF and final length were checked before publication.
    pub finished: bool,
}

pub struct RemoteBook {
    /// Fresh metadata, verified against the requested Telegram identity.
    pub book: BookAttachment,
    pub source_title: Option<String>,
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
    fn checkpoint(
        &self,
        record: &DownloadRecord,
    ) -> impl Future<Output = Result<Option<DownloadCheckpoint>, DownloadError>> + Send;
    fn save_checkpoint(
        &self,
        record: &DownloadRecord,
        checkpoint: &DownloadCheckpoint,
    ) -> impl Future<Output = Result<(), DownloadError>> + Send;
    fn clear_checkpoint(
        &self,
        record: &DownloadRecord,
    ) -> impl Future<Output = Result<(), DownloadError>> + Send;
    /// Explicit user intent may create another local copy of a completed book.
    fn claim_again(
        &self,
        _record: &DownloadRecord,
    ) -> impl Future<Output = Result<bool, DownloadError>> + Send {
        async { Err(DownloadError::Storage) }
    }
}

pub trait DownloadEngine: Clone + Send + Sync + 'static {
    fn recover(&self) -> impl Future<Output = Result<(), DownloadError>> + Send;
    /// Remove a waiting job's partial data only on explicit user cancellation.
    fn discard(
        &self,
        _job: &QueuedDownload,
    ) -> impl Future<Output = Result<(), DownloadError>> + Send {
        async { Ok(()) }
    }
    fn save(
        &self,
        remote: RemoteBook,
        directory: PathBuf,
        updates: Sender<DownloadUpdate>,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<DownloadRecord, DownloadError>> + Send;
    fn save_again(
        &self,
        _remote: RemoteBook,
        _directory: PathBuf,
        _updates: Sender<DownloadUpdate>,
        _cancellation: CancellationToken,
    ) -> impl Future<Output = Result<DownloadRecord, DownloadError>> + Send {
        async { Err(DownloadError::UnsupportedReply) }
    }
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
    /// A previous attempt started this job; a verified prefix may be reusable.
    pub interrupted: bool,
    /// Only an explicit user action bypasses completed-history deduplication.
    pub redownload: bool,
}

/// Durable queue intent, separate from the history of started file transfers.
pub trait QueueStore: Send + Sync {
    fn load_queue(
        &self,
        account: i64,
    ) -> impl Future<Output = Result<Vec<QueuedDownload>, DownloadError>> + Send;
    /// Atomically replace this account's ordered snapshot, including its active
    /// job (marked interrupted). Must leave the old snapshot intact on failure.
    fn save_queue(
        &self,
        account: i64,
        jobs: &[QueuedDownload],
    ) -> impl Future<Output = Result<(), DownloadError>> + Send;
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
    pub restored: bool,
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
