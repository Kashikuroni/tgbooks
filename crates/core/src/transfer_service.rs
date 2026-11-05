use std::{collections::HashSet, path::PathBuf, time::Duration};

use async_channel::Sender;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    AuthStep, Authentication, BookAttachment, BookSource, CatalogError, CatalogState,
    ChannelCatalog, CoreEvent, DownloadEngine, DownloadError, DownloadKey, DownloadRecord,
    DownloadStatus, DownloadUpdate, MAX_PENDING_DOWNLOADS, QueuedDownload, TransferState,
    auth::AuthController,
};

pub(super) type TaskResult = Result<Result<DownloadRecord, DownloadError>, JoinError>;

pub(super) struct RunningDownload {
    pub book: BookAttachment,
    cancellation: CancellationToken,
    task: JoinHandle<Result<DownloadRecord, DownloadError>>,
}

impl RunningDownload {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

pub(super) async fn wait(running: &mut Option<RunningDownload>) -> TaskResult {
    match running {
        Some(job) => (&mut job.task).await,
        None => std::future::pending().await,
    }
}

pub(super) async fn stop(running: Option<RunningDownload>) {
    if let Some(mut job) = running {
        job.cancel();
        // GPUI's quit hook has a small budget; startup recovery handles interrupted rows.
        if tokio::time::timeout(Duration::from_millis(100), &mut job.task)
            .await
            .is_err()
        {
            job.task.abort();
            let _ = job.task.await;
        }
    }
}

/// Owned by the Core actor. Waiting jobs never spawn workers or open Telegram files.
#[derive(Default)]
pub(super) struct DownloadQueue {
    pub state: TransferState,
    completed: HashSet<DownloadKey>,
}

impl DownloadQueue {
    pub fn reject(&mut self, error: DownloadError) {
        self.state.error = Some(error);
        self.state.error_file = None;
        self.state.skipped_duplicates = 0;
    }

    pub fn enqueue(
        &mut self,
        keys: Vec<DownloadKey>,
        directory: PathBuf,
        auth: &AuthController<impl Authentication>,
        catalog: &CatalogState,
        running: &Option<RunningDownload>,
        history: &[DownloadRecord],
    ) {
        if auth.state().step != AuthStep::Authorized {
            return self.reject(DownloadError::Unauthorized);
        }
        if !directory.is_absolute() {
            return self.reject(DownloadError::Destination);
        }
        if keys.is_empty() {
            return self.reject(DownloadError::Unavailable);
        }
        if keys.len() > MAX_PENDING_DOWNLOADS {
            return self.reject(DownloadError::QueueFull);
        }
        let mut seen = self.completed.clone();
        seen.extend(
            history
                .iter()
                .filter(|record| record.status == DownloadStatus::Completed)
                .map(|record| record.key),
        );
        seen.extend(running.as_ref().map(|job| job.book.key));
        seen.extend(self.state.queued.iter().map(|job| job.book.key));
        let mut additions = Vec::new();
        let mut skipped = 0;
        for key in keys {
            if !seen.insert(key) {
                skipped += 1;
                continue;
            }
            if catalog
                .channels
                .iter()
                .any(|channel| channel.id == key.channel_id && channel.protected)
            {
                return self.reject(DownloadError::ProtectedContent);
            }
            let Some(book) = catalog.books.iter().find(|book| book.key == key) else {
                return self.reject(DownloadError::Unavailable);
            };
            additions.push(QueuedDownload {
                book: book.clone(),
                directory: directory.clone(),
            });
        }
        if self.state.queued.len() + additions.len() > MAX_PENDING_DOWNLOADS {
            return self.reject(DownloadError::QueueFull);
        }
        // Admission is all-or-nothing, except duplicates which are intentionally skipped.
        self.state.queued.extend(additions);
        self.state.skipped_duplicates = skipped;
        self.state.error = None;
        self.state.error_file = None;
        if let Some(seconds) = auth.remaining_wait() {
            self.state.paused = true;
            self.state.error = Some(DownloadError::FloodWait(seconds));
        }
    }

    pub fn set_paused(&mut self, paused: bool, auth: &AuthController<impl Authentication>) {
        self.state.paused = paused;
        if !paused {
            let error = if auth.state().step != AuthStep::Authorized {
                Some(DownloadError::Unauthorized)
            } else {
                auth.remaining_wait().map(DownloadError::FloodWait)
            };
            self.state.paused = error.is_some();
            self.state.error = error;
            self.state.error_file = None;
        }
    }

    pub fn start_next(
        &mut self,
        auth: &mut AuthController<impl Authentication + BookSource>,
        catalog: &CatalogState,
        downloads: &impl DownloadEngine,
        progress: &Sender<DownloadUpdate>,
        running: &mut Option<RunningDownload>,
    ) {
        if running.is_some() || self.state.paused || self.state.queued.is_empty() {
            return;
        }
        if auth.state().step != AuthStep::Authorized || auth.remaining_wait().is_some() {
            self.set_paused(false, auth);
            return;
        }
        let queued = self.state.queued.remove(0);
        let key = queued.book.key;
        let protected = catalog
            .channels
            .iter()
            .any(|channel| channel.id == key.channel_id && channel.protected);
        let source = if protected {
            Box::pin(async { Err(DownloadError::ProtectedContent) }) as crate::BookFuture
        } else {
            auth.backend_mut().open_book(key)
        };
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let downloads = downloads.clone();
        let updates = progress.clone();
        let task = tokio::spawn(async move {
            let remote = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                result = source => result?,
            };
            if remote.book.key != key {
                return Err(DownloadError::InvalidFile);
            }
            downloads
                .save(remote, queued.directory, updates, cancel)
                .await
        });
        *running = Some(RunningDownload {
            book: queued.book,
            cancellation,
            task,
        });
    }

    pub async fn send(
        &self,
        running: &Option<RunningDownload>,
        events: &Sender<CoreEvent>,
    ) -> bool {
        let mut state = self.state.clone();
        state.active = running.as_ref().map(|job| job.book.key);
        state.cancelling = running
            .as_ref()
            .is_some_and(|job| job.cancellation.is_cancelled());
        events.send(CoreEvent::DownloadState(state)).await.is_ok()
    }

    pub async fn complete(
        &mut self,
        result: TaskResult,
        book: BookAttachment,
        downloads: &impl DownloadEngine,
        auth: &mut AuthController<impl Authentication + ChannelCatalog>,
        catalog: &mut CatalogState,
        events: &Sender<CoreEvent>,
    ) -> bool {
        let error = match result {
            Ok(Ok(record)) => {
                self.completed.insert(record.key);
                if events
                    .send(CoreEvent::DownloadUpdated(record))
                    .await
                    .is_err()
                {
                    return false;
                }
                None
            }
            Ok(Err(error)) => Some(error),
            Err(_) => {
                let _ = downloads.recover().await;
                Some(DownloadError::Internal)
            }
        };
        if let Some(error) = error {
            self.state.error = Some(error);
            self.state.error_file = Some(book.file_name);
            if matches!(
                error,
                DownloadError::FloodWait(_)
                    | DownloadError::Unauthorized
                    | DownloadError::SecureStorage
                    | DownloadError::Storage
                    | DownloadError::Internal
            ) {
                self.state.paused = true;
            }
            if let DownloadError::FloodWait(seconds) = error {
                auth.record_flood_wait(seconds);
            }
        }
        if error == Some(DownloadError::ProtectedContent) {
            if let Some(channel) = catalog
                .channels
                .iter_mut()
                .find(|channel| channel.id == book.key.channel_id)
            {
                channel.protected = true;
            }
            if catalog.selected == Some(book.key.channel_id) {
                catalog.reject(CatalogError::ProtectedContent);
            }
            if !super::send_catalog(catalog, events).await {
                return false;
            }
        }
        if error == Some(DownloadError::Unauthorized) {
            // Never carry jobs into a potentially different account after re-authentication.
            self.state.queued.clear();
            if !super::catalog_failure(auth, catalog, CatalogError::Unauthorized, events).await {
                return false;
            }
        }
        if catalog.error == Some(CatalogError::Busy) {
            catalog.error = None;
            if !super::send_catalog(catalog, events).await {
                return false;
            }
        }
        true
    }
}
