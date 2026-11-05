use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    AuthFailure, BookAttachment, BookChunks, BookFormat, BookFuture, BookPage, ChannelSummary,
    ChunkFuture, DownloadError, DownloadProgress, DownloadStatus, RemoteBook,
    downloads::UnusedDownloads,
};

use super::*;

fn key() -> DownloadKey {
    DownloadKey {
        channel_id: -1000000000001,
        message_id: 2,
        document_id: 3,
    }
}

fn book() -> BookAttachment {
    BookAttachment {
        key: key(),
        file_name: "book.epub".into(),
        size_bytes: Some(3),
        format: BookFormat::Epub,
    }
}

fn catalog() -> CatalogState {
    CatalogState {
        authenticated: true,
        channels: vec![ChannelSummary {
            id: key().channel_id,
            title: "Books".into(),
            username: None,
            protected: false,
        }],
        selected: Some(key().channel_id),
        books: vec![book()],
        scan_started: true,
        ..Default::default()
    }
}

#[derive(Default)]
struct FakeTelegram {
    calls: Arc<AtomicUsize>,
    error: Option<DownloadError>,
}

struct EmptyChunks;
impl BookChunks for EmptyChunks {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

impl BookSource for FakeTelegram {
    fn open_book(&self, key: DownloadKey) -> BookFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let error = self.error;
        Box::pin(async move {
            if let Some(error) = error {
                return Err(error);
            }
            let mut attachment = book();
            attachment.key = key;
            Ok(RemoteBook {
                book: attachment,
                chunks: Box::new(EmptyChunks),
            })
        })
    }
}

impl Authentication for FakeTelegram {
    async fn execute(&mut self, _: AuthAction) -> Result<AuthStep, AuthFailure> {
        Ok(AuthStep::Authorized)
    }
}
impl ChannelCatalog for FakeTelegram {
    async fn list_channels(&mut self) -> Result<Vec<ChannelSummary>, CatalogError> {
        Ok(catalog().channels)
    }
    async fn scan_books(&mut self, _: i64, _: Option<i32>) -> Result<BookPage, CatalogError> {
        Ok(BookPage {
            books: queue_catalog(4).books,
            scanned: 4,
            skipped_protected: 0,
            next_offset: None,
        })
    }
}

struct EmptyHistory;
impl DownloadHistory for EmptyHistory {
    type Error = Infallible;
    async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Default)]
struct BlockingEngine {
    stopped: Arc<AtomicBool>,
}
impl DownloadEngine for BlockingEngine {
    async fn recover(&self) -> Result<(), DownloadError> {
        Ok(())
    }
    async fn save(
        &self,
        remote: RemoteBook,
        _: PathBuf,
        updates: Sender<DownloadUpdate>,
        cancellation: CancellationToken,
    ) -> Result<DownloadRecord, DownloadError> {
        let _ = updates
            .send(
                DownloadRecord {
                    key: remote.book.key,
                    file_name: remote.book.file_name,
                    local_path: "books/book.epub".into(),
                    status: DownloadStatus::Downloading,
                    progress: DownloadProgress::new(0, Some(3))
                        .map_err(|_| DownloadError::InvalidFile)?,
                }
                .into(),
            )
            .await;
        cancellation.cancelled().await;
        self.stopped.store(true, Ordering::SeqCst);
        Err(DownloadError::Cancelled)
    }
}

async fn next_matching(
    ui: &UiConnection,
    predicate: impl Fn(&CoreEvent) -> bool,
) -> Result<CoreEvent, Box<dyn Error>> {
    Ok(tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = ui.events.recv().await?;
            if predicate(&event) {
                return Ok::<_, async_channel::RecvError>(event);
            }
        }
    })
    .await??)
}

#[tokio::test]
async fn unauthorized_protected_unknown_and_relative_requests_never_open_the_source() {
    let telegram = FakeTelegram::default();
    let calls = telegram.calls.clone();
    let mut auth = AuthController::new(telegram);
    let mut queue = transfers::DownloadQueue::default();
    let running = None;
    let mut selected = catalog();
    queue.enqueue(
        vec![key()],
        std::env::temp_dir(),
        &auth,
        &selected,
        &running,
        &[],
    );
    assert_eq!(queue.state.error, Some(DownloadError::Unauthorized));
    auth.complete(Ok(AuthStep::Authorized));
    selected.channels[0].protected = true;
    queue.enqueue(
        vec![key()],
        std::env::temp_dir(),
        &auth,
        &selected,
        &running,
        &[],
    );
    assert_eq!(queue.state.error, Some(DownloadError::ProtectedContent));
    selected.channels[0].protected = false;
    selected.books.clear();
    queue.enqueue(
        vec![key()],
        std::env::temp_dir(),
        &auth,
        &selected,
        &running,
        &[],
    );
    assert_eq!(queue.state.error, Some(DownloadError::Unavailable));
    queue.enqueue(
        vec![key()],
        "relative".into(),
        &auth,
        &catalog(),
        &running,
        &[],
    );
    assert_eq!(queue.state.error, Some(DownloadError::Destination));
    assert!(queue.state.queued.is_empty());
    assert!(running.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transfer_flood_wait_blocks_new_downloads_and_authentication_locally() {
    let telegram = FakeTelegram {
        error: Some(DownloadError::FloodWait(60)),
        ..Default::default()
    };
    let calls = telegram.calls.clone();
    let mut auth = AuthController::new(telegram);
    auth.complete(Ok(AuthStep::Authorized));
    let mut selected = catalog();
    let (events, _receiver) = async_channel::unbounded();
    let (progress, _progress_rx) = async_channel::unbounded();
    let mut running = None;
    let mut queue = transfers::DownloadQueue::default();
    queue.enqueue(
        vec![key()],
        std::env::temp_dir(),
        &auth,
        &selected,
        &running,
        &[],
    );
    queue.start_next(
        &mut auth,
        &selected,
        &UnusedDownloads,
        &progress,
        &mut running,
    );
    let result = transfers::wait(&mut running).await;
    running.take();
    assert!(
        queue
            .complete(
                result,
                book(),
                &UnusedDownloads,
                &mut auth,
                &mut selected,
                &events
            )
            .await
    );
    queue.enqueue(
        vec![key()],
        std::env::temp_dir(),
        &auth,
        &selected,
        &running,
        &[],
    );
    queue.set_paused(false, &auth);
    queue.start_next(
        &mut auth,
        &selected,
        &UnusedDownloads,
        &progress,
        &mut running,
    );
    assert!(queue.state.paused);
    assert_eq!(queue.state.queued.len(), 1);
    assert!(running.is_none());
    assert!(auth.prepare(AuthAction::Restore).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changed_protection_removes_catalogue_candidates_before_any_file_transfer() {
    let telegram = FakeTelegram {
        error: Some(DownloadError::ProtectedContent),
        ..Default::default()
    };
    let mut auth = AuthController::new(telegram);
    auth.complete(Ok(AuthStep::Authorized));
    let mut selected = catalog();
    let (events, _receiver) = async_channel::unbounded();
    let (progress, _progress_rx) = async_channel::unbounded();
    let mut running = None;
    let mut queue = transfers::DownloadQueue::default();
    queue.enqueue(
        vec![key()],
        std::env::temp_dir(),
        &auth,
        &selected,
        &running,
        &[],
    );
    queue.start_next(
        &mut auth,
        &selected,
        &UnusedDownloads,
        &progress,
        &mut running,
    );
    let result = transfers::wait(&mut running).await;
    running.take();
    assert!(
        queue
            .complete(
                result,
                book(),
                &UnusedDownloads,
                &mut auth,
                &mut selected,
                &events
            )
            .await
    );
    assert!(selected.books.is_empty());
    assert!(selected.channels[0].protected);
}

async fn running_actor()
-> Result<(UiConnection, tokio::task::JoinHandle<()>, Arc<AtomicBool>), Box<dyn Error>> {
    let (ui, backend) = connect();
    let engine = BlockingEngine::default();
    let stopped = engine.stopped.clone();
    let task = tokio::spawn(async {
        run_service(
            &EmptyHistory,
            FakeTelegram::default(),
            engine,
            AppInfo {
                data_directory: "data".into(),
                download_directory: "books".into(),
            },
            backend,
        )
        .await
    });
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy()),
    )
    .await?;
    ui.commands
        .send(CoreCommand::SelectChannel(Some(key().channel_id)))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_some()),
    )
    .await?;
    ui.commands.send(CoreCommand::ScanNextPage).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started),
    )
    .await?;
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: vec![key()],
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadUpdated(_))).await?;
    Ok((ui, task, stopped))
}

#[tokio::test]
async fn actor_reports_progress_rejects_parallel_scans_and_accepts_cancellation()
-> Result<(), Box<dyn Error>> {
    let (ui, task, stopped) = running_actor().await?;
    ui.commands.send(CoreCommand::RefreshChannels).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::Catalog(state) if state.error == Some(CatalogError::Busy))).await?;
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: vec![key()],
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.skipped_duplicates == 1 && state.active.is_some() && state.queued.is_empty())).await?;
    ui.commands.send(CoreCommand::CancelDownload(key())).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadState(state) if state.cancelling),
    )
    .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.error.is_none()),
    )
    .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(DownloadError::Cancelled) && state.active.is_none())).await?;
    assert!(stopped.load(Ordering::SeqCst));
    ui.commands.send(CoreCommand::Shutdown).await?;
    task.await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_token_cancels_and_joins_the_active_worker() -> Result<(), Box<dyn Error>> {
    let (ui, task, stopped) = running_actor().await?;
    ui.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), task).await??;
    assert!(
        stopped.load(Ordering::SeqCst),
        "the worker was detached instead of cancelled"
    );
    Ok(())
}

fn queue_catalog(count: usize) -> CatalogState {
    let mut state = catalog();
    state.books = (0..count)
        .map(|index| {
            let mut book = book();
            book.key.message_id += index as i32;
            book.key.document_id += index as i64;
            book.file_name = format!("book-{index}.epub");
            book
        })
        .collect();
    state
}

#[tokio::test]
async fn systemic_errors_pause_pending_jobs_and_revocation_discards_them() {
    for error in [
        DownloadError::FloodWait(60),
        DownloadError::SecureStorage,
        DownloadError::Storage,
        DownloadError::Internal,
        DownloadError::Unauthorized,
    ] {
        let telegram = FakeTelegram::default();
        let calls = telegram.calls.clone();
        let mut auth = AuthController::new(telegram);
        auth.complete(Ok(AuthStep::Authorized));
        let mut catalog = queue_catalog(2);
        let mut queue = transfers::DownloadQueue::default();
        queue.enqueue(
            catalog.books.iter().map(|book| book.key).collect(),
            std::env::temp_dir(),
            &auth,
            &catalog,
            &None,
            &[],
        );
        let first = queue.state.queued.remove(0);
        let (events, _receiver) = async_channel::unbounded();
        assert!(
            queue
                .complete(
                    Ok(Err(error)),
                    first.book,
                    &UnusedDownloads,
                    &mut auth,
                    &mut catalog,
                    &events
                )
                .await
        );
        assert!(queue.state.paused);
        assert_eq!(
            queue.state.queued.len(),
            usize::from(error != DownloadError::Unauthorized)
        );
        let (progress, _receiver) = async_channel::unbounded();
        let mut running = None;
        queue.start_next(
            &mut auth,
            &catalog,
            &UnusedDownloads,
            &progress,
            &mut running,
        );
        assert!(running.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if matches!(error, DownloadError::FloodWait(_)) {
            queue.set_paused(false, &auth);
            assert!(queue.state.paused);
            auth.record_flood_wait(0);
            assert!(
                queue.state.paused,
                "expiry must not silently resume the queue"
            );
            queue.set_paused(false, &auth);
            assert!(!queue.state.paused);
        }
    }
}

#[test]
fn batch_admission_is_atomic_bounded_and_skips_durable_duplicates() -> Result<(), Box<dyn Error>> {
    let mut auth = AuthController::new(FakeTelegram::default());
    auth.complete(Ok(AuthStep::Authorized));
    let mut queue = transfers::DownloadQueue::default();
    let catalog = queue_catalog(crate::MAX_PENDING_DOWNLOADS + 1);
    let first = catalog.books[0].key;
    let mut unknown = first;
    unknown.document_id = -1;
    queue.enqueue(
        vec![first, unknown],
        std::env::temp_dir(),
        &auth,
        &catalog,
        &None,
        &[],
    );
    assert!(
        queue.state.queued.is_empty(),
        "part of an invalid batch was admitted"
    );
    let completed = DownloadRecord {
        key: first,
        file_name: "book.epub".into(),
        local_path: "book.epub".into(),
        status: DownloadStatus::Completed,
        progress: DownloadProgress::new(3, Some(3))?,
    };
    queue.enqueue(
        vec![first, first, catalog.books[1].key],
        std::env::temp_dir(),
        &auth,
        &catalog,
        &None,
        &[completed],
    );
    assert_eq!(queue.state.queued.len(), 1);
    assert_eq!(queue.state.skipped_duplicates, 2);
    queue.enqueue(
        catalog
            .books
            .iter()
            .take(crate::MAX_PENDING_DOWNLOADS)
            .map(|book| book.key)
            .collect(),
        std::env::temp_dir(),
        &auth,
        &catalog,
        &None,
        &[],
    );
    assert_eq!(queue.state.queued.len(), crate::MAX_PENDING_DOWNLOADS);
    queue.enqueue(
        vec![catalog.books[crate::MAX_PENDING_DOWNLOADS].key],
        std::env::temp_dir(),
        &auth,
        &catalog,
        &None,
        &[],
    );
    assert_eq!(queue.state.error, Some(DownloadError::QueueFull));
    assert_eq!(queue.state.queued.len(), crate::MAX_PENDING_DOWNLOADS);
    Ok(())
}

#[derive(Clone)]
struct GatedEngine {
    release: async_channel::Receiver<Result<(), DownloadError>>,
    order: Arc<std::sync::Mutex<Vec<DownloadKey>>>,
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

impl DownloadEngine for GatedEngine {
    async fn recover(&self) -> Result<(), DownloadError> {
        Ok(())
    }
    async fn save(
        &self,
        remote: RemoteBook,
        _: PathBuf,
        updates: Sender<DownloadUpdate>,
        cancel: CancellationToken,
    ) -> Result<DownloadRecord, DownloadError> {
        let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(count, Ordering::SeqCst);
        self.order
            .lock()
            .map_err(|_| DownloadError::Internal)?
            .push(remote.book.key);
        let mut record = DownloadRecord {
            key: remote.book.key,
            file_name: remote.book.file_name,
            local_path: "books/book.epub".into(),
            status: DownloadStatus::Downloading,
            progress: DownloadProgress::new(0, Some(3)).map_err(|_| DownloadError::InvalidFile)?,
        };
        let _ = updates.send(record.clone().into()).await;
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(DownloadError::Cancelled),
            result = self.release.recv() => result.unwrap_or(Err(DownloadError::Network)),
        };
        self.active.fetch_sub(1, Ordering::SeqCst);
        result?;
        record.status = DownloadStatus::Completed;
        record.progress =
            DownloadProgress::new(3, Some(3)).map_err(|_| DownloadError::InvalidFile)?;
        Ok(record)
    }
}

async fn queue_actor(
    engine: GatedEngine,
) -> Result<(UiConnection, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    let (ui, backend) = connect();
    let task = tokio::spawn(async {
        run_service(
            &EmptyHistory,
            FakeTelegram::default(),
            engine,
            AppInfo {
                data_directory: "data".into(),
                download_directory: "books".into(),
            },
            backend,
        )
        .await;
    });
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy()),
    )
    .await?;
    ui.commands
        .send(CoreCommand::SelectChannel(Some(key().channel_id)))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_some()),
    )
    .await?;
    ui.commands.send(CoreCommand::ScanNextPage).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started),
    )
    .await?;
    Ok((ui, task))
}

fn gated_engine() -> (GatedEngine, Sender<Result<(), DownloadError>>) {
    let (release, receiver) = async_channel::unbounded();
    (
        GatedEngine {
            release: receiver,
            order: Default::default(),
            active: Default::default(),
            maximum: Default::default(),
        },
        release,
    )
}

#[tokio::test]
async fn fifo_accepts_more_jobs_skips_duplicates_cancels_waiting_and_continues_after_error()
-> Result<(), Box<dyn Error>> {
    let (engine, release) = gated_engine();
    let observed = engine.clone();
    let (ui, task) = queue_actor(engine).await?;
    let keys: Vec<_> = queue_catalog(4).books.iter().map(|book| book.key).collect();
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys[..2].to_vec(),
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[0]),
    )
    .await?;
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys.clone(),
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.queued.len() == 3 && state.skipped_duplicates == 2)).await?;
    ui.commands
        .send(CoreCommand::CancelDownload(keys[1]))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadState(state) if state.queued.len() == 2),
    )
    .await?;
    // Changing the displayed channel must not erase the admitted books.
    ui.commands.send(CoreCommand::SelectChannel(None)).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_none()),
    )
    .await?;
    release.send(Err(DownloadError::Network)).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[2]),
    )
    .await?;
    release.send(Ok(())).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[3]),
    )
    .await?;
    release.send(Ok(())).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_none() && state.queued.is_empty())).await?;
    assert_eq!(
        *observed.order.lock().map_err(|_| "poisoned test lock")?,
        vec![keys[0], keys[2], keys[3]]
    );
    assert_eq!(observed.maximum.load(Ordering::SeqCst), 1);
    // Successful jobs cannot be re-added even if the adapter returns stale history.
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: vec![keys[3]],
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.skipped_duplicates == 1 && state.queued.is_empty())).await?;
    ui.commands.send(CoreCommand::Shutdown).await?;
    task.await?;
    Ok(())
}

#[tokio::test]
async fn pause_finishes_current_file_and_resume_then_active_cancel_advances_fifo()
-> Result<(), Box<dyn Error>> {
    let (engine, release) = gated_engine();
    let observed = engine.clone();
    let (ui, task) = queue_actor(engine).await?;
    let keys: Vec<_> = queue_catalog(3).books.iter().map(|book| book.key).collect();
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys.clone(),
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[0]),
    )
    .await?;
    ui.commands.send(CoreCommand::SetQueuePaused(true)).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.paused && state.active == Some(keys[0]))).await?;
    release.send(Ok(())).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.paused && state.active.is_none() && state.queued.len() == 2)).await?;
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
    ui.commands.send(CoreCommand::SetQueuePaused(false)).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[1]),
    )
    .await?;
    ui.commands
        .send(CoreCommand::CancelDownload(keys[1]))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[2]),
    )
    .await?;
    release.send(Ok(())).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_none() && state.queued.is_empty())).await?;
    assert_eq!(
        *observed.order.lock().map_err(|_| "poisoned test lock")?,
        keys
    );
    assert_eq!(observed.maximum.load(Ordering::SeqCst), 1);
    ui.commands.send(CoreCommand::Shutdown).await?;
    task.await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_does_not_start_waiting_jobs() -> Result<(), Box<dyn Error>> {
    let (engine, _release) = gated_engine();
    let observed = engine.clone();
    let (ui, task) = queue_actor(engine).await?;
    let keys: Vec<_> = queue_catalog(3).books.iter().map(|book| book.key).collect();
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys.clone(),
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[0]),
    )
    .await?;
    ui.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), task).await??;
    assert_eq!(
        *observed.order.lock().map_err(|_| "poisoned test lock")?,
        vec![keys[0]]
    );
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
    Ok(())
}
