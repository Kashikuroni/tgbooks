use std::{collections::HashSet, path::PathBuf};

use async_channel::Sender;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    AuthStep, Authentication, BookAttachment, BookSource, CatalogError, CatalogState,
    ChannelCatalog, CoreEvent, DownloadEngine, DownloadError, DownloadHistory, DownloadKey,
    DownloadRecord, DownloadStatus, DownloadUpdate, MAX_PENDING_DOWNLOADS, QueueStore,
    QueuedDownload, TransferState, auth::AuthController,
};

pub(super) type TaskResult = Result<Result<DownloadRecord, DownloadError>, JoinError>;

pub(super) struct RunningDownload {
    pub entry: QueuedDownload,
    pub retain_on_shutdown: bool,
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
    if let Some(job) = running {
        // Shutdown is not explicit cancellation: preserve the last synced
        // checkpoint. Startup recovery marks interrupted history rows failed.
        job.task.abort();
        let _ = job.task.await;
    }
}

/// Owned by the Core actor. Waiting jobs never spawn workers or open Telegram files.
#[derive(Clone, Default)]
pub(super) struct DownloadQueue {
    pub state: TransferState,
    pub(super) owner: Option<i64>,
    completed: HashSet<DownloadKey>,
}

impl DownloadQueue {
    pub async fn restore(
        &mut self,
        store: &(impl QueueStore + DownloadHistory),
        auth: &AuthController<impl Authentication>,
    ) -> Result<bool, DownloadError> {
        let announce = self.state.error.is_some() || !self.state.queued.is_empty();
        let Some(owner) = auth.account_id() else {
            self.forget_account();
            return Err(DownloadError::Unauthorized);
        };
        if self.owner == Some(owner) {
            return Ok(false);
        }
        if self.owner.is_some() {
            self.forget_account();
        }
        let jobs = store.load_queue(owner).await?;
        let history = store
            .list_downloads()
            .await
            .map_err(|_| DownloadError::Storage)?;
        let completed: HashSet<_> = history
            .into_iter()
            .filter(|record| record.status == DownloadStatus::Completed)
            .map(|record| record.key)
            .collect();
        let jobs: Vec<_> = jobs
            .into_iter()
            .filter(|job| job.redownload || !completed.contains(&job.book.key))
            .collect();
        self.state = TransferState {
            paused: !jobs.is_empty(),
            restored: !jobs.is_empty(),
            queued: jobs,
            ..Default::default()
        };
        self.owner = Some(owner);
        self.completed = completed;
        Ok(!self.state.queued.is_empty() || announce)
    }

    pub fn forget_account(&mut self) {
        self.owner = None;
        self.completed.clear();
        self.state.queued.clear();
        self.state.restored = false;
        self.state.paused = true;
    }

    fn snapshot(&self, running: &Option<RunningDownload>) -> Vec<QueuedDownload> {
        let mut jobs = Vec::new();
        if let Some(active) = running.as_ref().filter(|job| job.retain_on_shutdown) {
            let mut entry = active.entry.clone();
            entry.interrupted = true;
            jobs.push(entry);
        }
        jobs.extend(self.state.queued.clone());
        jobs
    }

    async fn save_snapshot(&mut self, store: &impl QueueStore, jobs: &[QueuedDownload]) -> bool {
        let result = match self.owner {
            Some(owner) => store.save_queue(owner, jobs).await,
            None => Err(DownloadError::Unauthorized),
        };
        if let Err(error) = result {
            self.reject(error);
            self.state.paused = true;
            return false;
        }
        true
    }

    pub async fn persist(
        &mut self,
        store: &impl QueueStore,
        running: &Option<RunningDownload>,
    ) -> bool {
        self.save_snapshot(store, &self.snapshot(running)).await
    }

    pub async fn commit(
        &mut self,
        mut next: Self,
        store: &impl QueueStore,
        running: &Option<RunningDownload>,
    ) {
        if next.persist(store, running).await {
            *self = next;
        } else {
            self.reject(next.state.error.unwrap_or(DownloadError::Storage));
            self.state.paused = true;
        }
    }

    pub async fn cancel(
        &mut self,
        key: DownloadKey,
        store: &impl QueueStore,
        downloads: &impl DownloadEngine,
        running: &mut Option<RunningDownload>,
    ) {
        if let Some(active) = running.as_mut().filter(|job| job.entry.book.key == key) {
            if self.save_snapshot(store, &self.state.queued.clone()).await {
                active.retain_on_shutdown = false;
                active.cancel();
            }
        } else {
            let mut next = self.clone();
            if let Some(job) = next.state.queued.iter().find(|job| job.book.key == key)
                && let Err(error) = downloads.discard(job).await
            {
                // An unavailable drive must not trap an unwanted job forever.
                // Report failed cleanup, but still remove durable queue intent.
                // Its checkpoint remains; never delete a different path.
                next.reject(error);
            }
            next.state.queued.retain(|job| job.book.key != key);
            if next.state.queued != self.state.queued {
                self.commit(next, store, running).await;
            }
        }
    }

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
        self.enqueue_with_policy(keys, directory, auth, catalog, running, (history, false));
    }

    pub fn enqueue_with_policy(
        &mut self,
        keys: Vec<DownloadKey>,
        directory: PathBuf,
        auth: &AuthController<impl Authentication>,
        catalog: &CatalogState,
        running: &Option<RunningDownload>,
        (history, redownload): (&[DownloadRecord], bool),
    ) {
        if auth
            .account_id()
            .is_none_or(|owner| self.owner != Some(owner))
        {
            return self.reject(DownloadError::Unauthorized);
        }
        if !directory.is_absolute() || directory.to_str().is_none() {
            return self.reject(DownloadError::Destination);
        }
        if keys.is_empty() {
            return self.reject(DownloadError::Unavailable);
        }
        if keys.len() > MAX_PENDING_DOWNLOADS {
            return self.reject(DownloadError::QueueFull);
        }
        let mut seen = HashSet::new();
        if !redownload {
            seen.extend(self.completed.iter().copied());
            seen.extend(
                history
                    .iter()
                    .filter(|record| record.status == DownloadStatus::Completed)
                    .map(|record| record.key),
            );
        }
        seen.extend(running.as_ref().map(|job| job.entry.book.key));
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
                interrupted: false,
                redownload,
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
            if !self.state.paused {
                self.state.restored = false;
            }
        }
    }

    pub async fn start_next(
        &mut self,
        store: &impl QueueStore,
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
        if self.owner != auth.account_id() || self.owner.is_none() {
            self.forget_account();
            self.reject(DownloadError::Unauthorized);
            return;
        }
        if catalog.channels.is_empty() && catalog.error.is_some() {
            // A failed startup listing is not evidence that every saved file
            // disappeared. Keep intent until the user refreshes the catalogue.
            self.state.paused = true;
            self.reject(DownloadError::Network);
            return;
        }
        // Durable intent precedes the first network/file operation. Shutdown
        // does not need a final SQLite write within GPUI's short quit budget.
        let mut snapshot = self.state.queued.clone();
        snapshot[0].interrupted = true;
        if !self.save_snapshot(store, &snapshot).await {
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
        let directory = queued.directory.clone();
        let redownload = queued.redownload;
        let interrupted = queued.interrupted;
        let task = tokio::spawn(async move {
            // A previous attempt may have lost its final "failed" write when
            // SQLite was unavailable. There is no other active worker here.
            if interrupted {
                downloads.recover().await?;
            }
            let remote = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                result = source => result?,
            };
            if remote.book.key != key {
                return Err(DownloadError::InvalidFile);
            }
            if redownload {
                downloads
                    .save_again(remote, directory, updates, cancel)
                    .await
            } else {
                downloads.save(remote, directory, updates, cancel).await
            }
        });
        *running = Some(RunningDownload {
            entry: queued,
            retain_on_shutdown: true,
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
        state.active = running.as_ref().map(|job| job.entry.book.key);
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
                DownloadError::Network
                    | DownloadError::FloodWait(_)
                    | DownloadError::Unauthorized
                    | DownloadError::SecureStorage
                    | DownloadError::Storage
                    | DownloadError::Destination
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
            self.forget_account();
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
