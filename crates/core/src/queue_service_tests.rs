//! Durable-intent protocol tests; no network, disk or platform UI.
use std::{collections::HashMap, sync::Mutex};

use crate::QueuedDownload;

use super::*;

#[derive(Clone, Default)]
struct QueueMemory {
    jobs: Arc<Mutex<HashMap<i64, Vec<QueuedDownload>>>>,
    fail_load: Arc<AtomicBool>,
    fail_save: Arc<AtomicBool>,
    loads: Arc<AtomicUsize>,
    saves: Arc<AtomicUsize>,
}

impl crate::SourceStore for QueueMemory {}
impl crate::RecommendationStore for QueueMemory {}

impl crate::SettingsStore for QueueMemory {
    async fn save_settings(
        &self,
        settings: &crate::AppSettings,
    ) -> Result<crate::AppSettings, crate::SettingsError> {
        if self.fail_save.load(Ordering::SeqCst) {
            Err(crate::SettingsError::Storage)
        } else {
            Ok(settings.clone())
        }
    }
}

impl DownloadHistory for QueueMemory {
    type Error = Infallible;
    async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
        Ok(Vec::new())
    }
}

impl QueueStore for QueueMemory {
    async fn load_queue(&self, account: i64) -> Result<Vec<QueuedDownload>, DownloadError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        if self.fail_load.load(Ordering::SeqCst) {
            return Err(DownloadError::Storage);
        }
        Ok(self
            .jobs
            .lock()
            .map_err(|_| DownloadError::Storage)?
            .get(&account)
            .cloned()
            .unwrap_or_default())
    }
    async fn save_queue(&self, account: i64, jobs: &[QueuedDownload]) -> Result<(), DownloadError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        if self.fail_save.load(Ordering::SeqCst) {
            return Err(DownloadError::Storage);
        }
        self.jobs
            .lock()
            .map_err(|_| DownloadError::Storage)?
            .insert(account, jobs.to_vec());
        Ok(())
    }
}

struct OwnedTelegram {
    owner: Option<i64>,
    inner: FakeTelegram,
}
impl Authentication for OwnedTelegram {
    fn account_id(&self) -> Option<i64> {
        self.owner
    }
    async fn execute(&mut self, action: AuthAction) -> Result<AuthStep, AuthFailure> {
        self.inner.execute(action).await
    }
}
impl ChannelCatalog for OwnedTelegram {
    async fn list_channels(&mut self) -> Result<Vec<ChannelSummary>, CatalogError> {
        self.inner.list_channels().await
    }
    async fn scan_books(
        &mut self,
        channel: i64,
        before: Option<i32>,
    ) -> Result<BookPage, CatalogError> {
        self.inner.scan_books(channel, before).await
    }
}
impl BookSource for OwnedTelegram {
    fn open_book(&self, key: DownloadKey) -> BookFuture {
        self.inner.open_book(key)
    }
}

fn entries(count: usize) -> Vec<QueuedDownload> {
    queue_catalog(count)
        .books
        .into_iter()
        .map(|book| QueuedDownload {
            book,
            directory: std::env::temp_dir(),
            interrupted: false,
            redownload: false,
        })
        .collect()
}

async fn boot(
    store: QueueMemory,
    engine: GatedEngine,
    owner: Option<i64>,
) -> Result<(UiConnection, tokio::task::JoinHandle<()>, Arc<AtomicUsize>), Box<dyn Error>> {
    let (ui, backend) = connect();
    let inner = FakeTelegram::default();
    let calls = inner.calls.clone();
    let task = tokio::spawn(async move {
        run_service(
            &store,
            OwnedTelegram { owner, inner },
            engine,
            AppInfo {
                settings: crate::AppSettings::new("books".into()),
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
    Ok((ui, task, calls))
}

async fn scan(ui: &UiConnection) -> Result<(), Box<dyn Error>> {
    ui.commands
        .send(CoreCommand::SelectChannel(Some(key().channel_id)))
        .await?;
    next_matching(
        ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_some()),
    )
    .await?;
    next_matching(
        ui,
        |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn settings_acknowledgement_controls_default_folder_and_discovery_but_not_accepted_jobs()
-> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    let (engine, _release) = gated_engine();
    let (ui, task, _) = boot(store.clone(), engine, Some(1)).await?;
    scan(&ui).await?;
    ui.commands.send(CoreCommand::SetQueuePaused(true)).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadState(state) if state.paused),
    )
    .await?;
    let directory = std::env::temp_dir().join("accepted-folder");
    let accepted = crate::AppSettings::new(directory.clone());
    ui.commands
        .send(CoreCommand::SaveSettings(accepted.clone()))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::SettingsSaved(Ok(value)) if value == &accepted),
    )
    .await?;
    let rejected = crate::AppSettings::new(std::env::temp_dir().join("rejected-folder"));
    store.fail_save.store(true, Ordering::SeqCst);
    ui.commands
        .send(CoreCommand::SaveSettings(rejected))
        .await?;
    next_matching(&ui, |event| {
        matches!(
            event,
            CoreEvent::SettingsSaved(Err(crate::SettingsError::Storage))
        )
    })
    .await?;
    store.fail_save.store(false, Ordering::SeqCst);
    ui.commands
        .send(CoreCommand::DownloadToDefault(vec![key()]))
        .await?;
    next_matching(&ui, |event| {
        matches!(event, CoreEvent::DownloadState(state)
        if state.queued.len() == 1 && state.queued[0].directory == directory)
    })
    .await?;
    let Some(pdf_only) = crate::BookFormats::from_mask(1) else {
        panic!("pdf");
    };
    let changed = crate::AppSettings {
        download_directory: std::env::temp_dir().join("next-folder"),
        formats: pdf_only,
        system_notifications: false,
    };
    ui.commands.send(CoreCommand::SaveSettings(changed)).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::Catalog(state)
        if state.formats == pdf_only && state.scan_started && !state.busy() && state.books.is_empty())).await?;
    assert_eq!(store.load_queue(1).await?[0].directory, directory);
    stop(ui, task).await
}

async fn stop(ui: UiConnection, task: tokio::task::JoinHandle<()>) -> Result<(), Box<dyn Error>> {
    ui.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), task).await??;
    Ok(())
}

#[tokio::test]
async fn interrupted_and_waiting_jobs_restore_paused_in_order_until_manual_resume()
-> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    let jobs = entries(3);
    let keys: Vec<_> = jobs.iter().map(|job| job.book.key).collect();
    let (engine, _release) = gated_engine();
    let (ui, task, _) = boot(store.clone(), engine, Some(1)).await?;
    scan(&ui).await?;
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
    let saved = store.load_queue(1).await?;
    assert_eq!(saved.len(), 3);
    assert!(saved[0].interrupted);
    assert!(!saved[1].interrupted);
    stop(ui, task).await?;

    let (engine, _release) = gated_engine();
    let (ui, task, calls) = boot(store.clone(), engine, Some(1)).await?;
    let restored = next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadState(state) if state.restored),
    )
    .await?;
    assert!(
        matches!(restored, CoreEvent::DownloadState(state) if state.paused && state.active.is_none()
        && state.queued == saved)
    );
    ui.commands.send(CoreCommand::RefreshHistory).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::HistoryLoaded(_))).await?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "restoration opened a file before manual resume"
    );
    ui.commands
        .send(CoreCommand::CancelDownload(keys[1]))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadState(state) if state.queued.len() == 2),
    )
    .await?;
    stop(ui, task).await?;
    assert_eq!(
        store
            .load_queue(1)
            .await?
            .iter()
            .map(|job| job.book.key)
            .collect::<Vec<_>>(),
        vec![keys[0], keys[2]]
    );

    let (engine, release) = gated_engine();
    let observed = engine.clone();
    let (ui, task, calls) = boot(store.clone(), engine, Some(1)).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.restored && state.queued.len() == 2)).await?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // No channel selection or history re-scan is needed for admitted metadata.
    ui.commands.send(CoreCommand::SetQueuePaused(false)).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[0] && record.progress.downloaded_bytes() == 0)).await?;
    release.send(Ok(())).await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[2]),
    )
    .await?;
    release.send(Ok(())).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_none() && state.queued.is_empty())).await?;
    assert!(store.load_queue(1).await?.is_empty());
    assert_eq!(
        *observed.order.lock().map_err(|_| "test lock")?,
        vec![keys[0], keys[2]]
    );
    assert_eq!(observed.maximum.load(Ordering::SeqCst), 1);
    stop(ui, task).await?;
    Ok(())
}

#[tokio::test]
async fn explicit_active_cancellation_is_removed_before_shutdown() -> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    let keys: Vec<_> = entries(2).iter().map(|job| job.book.key).collect();
    let (engine, _release) = gated_engine();
    let (ui, task, _) = boot(store.clone(), engine, Some(1)).await?;
    scan(&ui).await?;
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
    ui.commands
        .send(CoreCommand::CancelDownload(keys[0]))
        .await?;
    next_matching(
        &ui,
        |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == keys[1]),
    )
    .await?;
    stop(ui, task).await?;
    let saved = store.load_queue(1).await?;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].book.key, keys[1]);
    assert!(saved[0].interrupted);
    Ok(())
}

#[tokio::test]
async fn failed_admission_dispatch_and_removal_never_acknowledge_unsaved_changes()
-> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    let keys: Vec<_> = entries(2).iter().map(|job| job.book.key).collect();
    let (engine, _release) = gated_engine();
    let (ui, task, calls) = boot(store.clone(), engine, Some(1)).await?;
    scan(&ui).await?;
    store.fail_save.store(true, Ordering::SeqCst);
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys.clone(),
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(DownloadError::Storage) && state.queued.is_empty())).await?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(store.load_queue(1).await?.is_empty());
    store.fail_save.store(false, Ordering::SeqCst);
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys.clone(),
            directory: std::env::temp_dir(),
        })
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.paused && state.queued.len() == 2)).await?;
    let before = store.load_queue(1).await?;
    store.fail_save.store(true, Ordering::SeqCst);
    ui.commands.send(CoreCommand::SetQueuePaused(false)).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(DownloadError::Storage) && state.paused && state.queued.len() == 2)).await?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    ui.commands
        .send(CoreCommand::CancelDownload(keys[1]))
        .await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(DownloadError::Storage) && state.queued.len() == 2)).await?;
    assert_eq!(store.load_queue(1).await?, before);
    stop(ui, task).await?;
    Ok(())
}

#[tokio::test]
async fn failed_restore_cannot_be_overwritten_by_enqueue_or_resume_and_can_be_retried()
-> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    let jobs = entries(2);
    store.save_queue(1, &jobs).await?;
    store.fail_load.store(true, Ordering::SeqCst);
    let (engine, _release) = gated_engine();
    let (ui, task, calls) = boot(store.clone(), engine, Some(1)).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(DownloadError::Storage))).await?;
    for command in [
        CoreCommand::DownloadBooks {
            keys: vec![jobs[0].book.key],
            directory: std::env::temp_dir(),
        },
        CoreCommand::SetQueuePaused(false),
    ] {
        ui.commands.send(command).await?;
        next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(DownloadError::Storage) && state.paused)).await?;
    }
    assert_eq!(
        store.saves.load(Ordering::SeqCst),
        1,
        "unread intent was overwritten"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    store.fail_load.store(false, Ordering::SeqCst);
    ui.commands.send(CoreCommand::RefreshHistory).await?;
    next_matching(&ui, |event| matches!(event, CoreEvent::DownloadState(state) if state.restored && state.paused && state.queued == jobs)).await?;
    assert_eq!(store.load_queue(1).await?, jobs);
    stop(ui, task).await?;
    Ok(())
}

#[tokio::test]
async fn failed_startup_listing_does_not_discard_saved_jobs() -> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    store.save_queue(1, &entries(2)).await?;
    let inner = FakeTelegram::default();
    let calls = inner.calls.clone();
    let mut auth = AuthController::new(inner);
    auth.complete(Ok(AuthStep::Authorized));
    let mut queue = transfers::DownloadQueue::default();
    queue.restore(&store, &auth).await?;
    queue.set_paused(false, &auth);
    let failed_catalog = CatalogState {
        authenticated: true,
        error: Some(CatalogError::Network),
        ..Default::default()
    };
    let (progress, _receiver) = async_channel::unbounded();
    let mut running = None;
    queue
        .start_next(
            &store,
            &mut auth,
            &failed_catalog,
            &UnusedDownloads,
            &progress,
            &mut running,
        )
        .await;
    assert!(running.is_none());
    assert!(queue.state.paused);
    assert_eq!(queue.state.queued, entries(2));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.load_queue(1).await?, entries(2));
    Ok(())
}

#[tokio::test]
async fn unavailable_partial_cleanup_does_not_trap_a_cancelled_job() -> Result<(), Box<dyn Error>> {
    #[derive(Clone)]
    struct UnavailableDestination;
    impl DownloadEngine for UnavailableDestination {
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
        async fn discard(&self, _: &QueuedDownload) -> Result<(), DownloadError> {
            Err(DownloadError::Destination)
        }
    }
    let store = QueueMemory::default();
    let mut jobs = entries(1);
    jobs[0].interrupted = true;
    store.save_queue(1, &jobs).await?;
    let mut auth = AuthController::new(FakeTelegram::default());
    auth.complete(Ok(AuthStep::Authorized));
    let mut queue = transfers::DownloadQueue::default();
    queue.restore(&store, &auth).await?;
    queue
        .cancel(jobs[0].book.key, &store, &UnavailableDestination, &mut None)
        .await;
    assert!(queue.state.queued.is_empty());
    assert_eq!(queue.state.error, Some(DownloadError::Destination));
    assert!(store.load_queue(1).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn account_binding_never_reuses_another_accounts_or_unverified_jobs()
-> Result<(), Box<dyn Error>> {
    let store = QueueMemory::default();
    store.save_queue(1, &entries(2)).await?;
    store.save_queue(2, &entries(1)).await?;
    let mut auth = AuthController::new(OwnedTelegram {
        owner: Some(1),
        inner: FakeTelegram::default(),
    });
    auth.complete(Ok(AuthStep::Authorized));
    let mut queue = transfers::DownloadQueue::default();
    assert!(queue.restore(&store, &auth).await?);
    assert_eq!(queue.state.queued.len(), 2);
    auth.backend_mut().owner = Some(2);
    assert!(queue.restore(&store, &auth).await?);
    assert_eq!(queue.owner, Some(2));
    assert_eq!(queue.state.queued.len(), 1);
    auth.backend_mut().owner = None;
    let reads = store.loads.load(Ordering::SeqCst);
    assert_eq!(
        queue.restore(&store, &auth).await,
        Err(DownloadError::Unauthorized)
    );
    assert!(queue.state.queued.is_empty());
    assert_eq!(store.loads.load(Ordering::SeqCst), reads);
    assert_eq!(store.load_queue(1).await?.len(), 2);
    Ok(())
}
