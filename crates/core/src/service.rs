use std::{error::Error, future::Future, path::PathBuf};

use async_channel::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

use crate::{
    AppSettings, AuthAction, AuthState, AuthStep, Authentication, BookSource, CatalogError,
    CatalogOperation, CatalogState, ChannelCatalog, DownloadEngine, DownloadKey, DownloadRecord,
    DownloadUpdate, QueueStore, RecommendationError, RecommendationState, RecommendationStatus,
    RecommendationStore, SettingsError, SettingsStore, SourceError, SourceStore, TransferMetrics,
    TransferState, auth::AuthController,
};

#[path = "monitor_service.rs"]
mod monitoring;
#[path = "source_service.rs"]
mod sources;
#[path = "transfer_service.rs"]
mod transfers;

/// This port is implemented by storage. Tests can supply an in-memory adapter.
pub trait DownloadHistory: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn list_downloads(
        &self,
    ) -> impl Future<Output = Result<Vec<DownloadRecord>, Self::Error>> + Send;
}

#[derive(Debug)]
pub enum CoreCommand {
    SetMonitoring {
        account: i64,
        enabled: bool,
    },
    ReloadRecommendations,
    ChangeRecommendations {
        account: i64,
        keys: Vec<DownloadKey>,
        status: RecommendationStatus,
    },
    DownloadRecommendations {
        account: i64,
        keys: Vec<DownloadKey>,
    },
    RefreshHistory,
    SaveSettings(AppSettings),
    DownloadToDefault(Vec<DownloadKey>),
    Authenticate(AuthAction),
    RefreshChannels,
    ReloadSources,
    SetChannelSource {
        id: i64,
        enabled: bool,
    },
    SelectChannel(Option<i64>),
    ScanNextPage,
    DownloadBooks {
        keys: Vec<DownloadKey>,
        directory: PathBuf,
    },
    RedownloadBook {
        key: DownloadKey,
        /// None resolves the last committed default directory inside Core.
        directory: Option<PathBuf>,
    },
    CancelDownload(DownloadKey),
    SetQueuePaused(bool),
    Shutdown,
}

/// Display-only paths supplied by the platform adapter, not resolved by Core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppInfo {
    pub data_directory: String,
    pub download_directory: String,
    pub settings: AppSettings,
}

/// Only safe, typed errors cross the UI boundary, never raw API/SQL errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceFailure {
    Initialization,
    AlreadyRunning,
    Storage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreEvent {
    RecommendationBatch(crate::RecommendationBatch),
    Recommendations(RecommendationState),
    RecommendationActionFinished,
    Ready(AppInfo),
    SettingsSaved(Result<AppSettings, SettingsError>),
    SourceSaved(Result<(), SourceError>),
    HistoryLoaded(Vec<DownloadRecord>),
    Authentication(AuthState),
    Catalog(CatalogState),
    DownloadState(TransferState),
    DownloadUpdated(DownloadRecord),
    DownloadMetrics {
        key: DownloadKey,
        metrics: TransferMetrics,
    },
    Failed(ServiceFailure),
}

/// Both channel ends work on Tokio and GPUI's executor, without polling timers.
pub struct UiConnection {
    pub commands: Sender<CoreCommand>,
    pub events: Receiver<CoreEvent>,
    pub shutdown: CancellationToken,
}

pub struct ServiceConnection {
    pub commands: Receiver<CoreCommand>,
    pub events: Sender<CoreEvent>,
    pub shutdown: CancellationToken,
}

pub fn connect() -> (UiConnection, ServiceConnection) {
    let (commands_tx, commands_rx) = async_channel::bounded(16);
    let (events_tx, events_rx) = async_channel::bounded(16);
    let shutdown = CancellationToken::new();
    (
        UiConnection {
            commands: commands_tx,
            events: events_rx,
            shutdown: shutdown.clone(),
        },
        ServiceConnection {
            commands: commands_rx,
            events: events_tx,
            shutdown,
        },
    )
}

async fn load_history(history: &impl DownloadHistory, events: &Sender<CoreEvent>) -> bool {
    let event = match history.list_downloads().await {
        Ok(records) => CoreEvent::HistoryLoaded(records),
        Err(_) => CoreEvent::Failed(ServiceFailure::Storage),
    };
    events.send(event).await.is_ok()
}

/// One owner processes commands in order. Dropping the UI closes its command
/// sender and event receiver; closing commands explicitly also stops the loop.
pub async fn run_service(
    history: &(impl DownloadHistory + QueueStore + SettingsStore + SourceStore + RecommendationStore),
    authentication: impl Authentication + ChannelCatalog + BookSource,
    downloads: impl DownloadEngine,
    info: AppInfo,
    connection: ServiceConnection,
) {
    let shutdown = connection.shutdown.clone();
    let mut running = None;
    // Cancellation is separate from the bounded command queue: quitting must
    // interrupt an in-flight RPC, keychain prompt or SQLite query immediately.
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {}
        _ = run_loop(history, authentication, downloads, info, connection, &mut running) => {}
    }
    transfers::stop(running).await;
}

async fn authenticate(
    auth: &mut AuthController<impl Authentication>,
    action: AuthAction,
    events: &Sender<CoreEvent>,
) -> bool {
    let action = auth.prepare(action);
    if events
        .send(CoreEvent::Authentication(auth.state()))
        .await
        .is_err()
    {
        return false;
    }
    if let Some(action) = action {
        let result = auth.execute(action).await;
        auth.complete(result);
        return events
            .send(CoreEvent::Authentication(auth.state()))
            .await
            .is_ok();
    }
    true
}

async fn run_loop(
    history: &(impl DownloadHistory + QueueStore + SettingsStore + SourceStore + RecommendationStore),
    authentication: impl Authentication + ChannelCatalog + BookSource,
    downloads: impl DownloadEngine,
    info: AppInfo,
    connection: ServiceConnection,
    running: &mut Option<transfers::RunningDownload>,
) {
    let mut settings = info.settings.clone();
    if connection
        .events
        .send(CoreEvent::Ready(info))
        .await
        .is_err()
    {
        return;
    }
    if downloads.recover().await.is_err() {
        let _ = connection
            .events
            .send(CoreEvent::Failed(ServiceFailure::Storage))
            .await;
        return;
    }
    if !load_history(history, &connection.events).await {
        return;
    }
    let mut auth = AuthController::new(authentication);
    let mut catalog = CatalogState {
        formats: settings.formats,
        ..Default::default()
    };
    if !authenticate(&mut auth, AuthAction::Restore, &connection.events).await {
        return;
    }
    sources::sync(history, &auth, &mut catalog, false).await;
    if !catalog_after_auth(&mut auth, &mut catalog, &connection.events).await {
        return;
    }

    let (progress, progress_rx) = async_channel::bounded::<DownloadUpdate>(16);
    let mut queue = transfers::DownloadQueue::default();
    if !restore_queue(history, &auth, &mut queue, running, &connection.events).await {
        return;
    }
    let mut monitor = monitoring::Monitor::default();
    loop {
        if queue.owner.is_some()
            && queue.owner != auth.account_id()
            && !restore_queue(history, &auth, &mut queue, running, &connection.events).await
        {
            break;
        }
        let sources_changed = sources::sync(history, &auth, &mut catalog, false).await;
        if sources_changed && !send_catalog(&catalog, &connection.events).await {
            break;
        }
        let inbox_changed = monitor.sync(history, &auth, &catalog, false).await;
        if (inbox_changed || sources_changed) && !monitor.send(&connection.events).await {
            break;
        }
        // Background RPCs run only between downloads and yield to ready UI commands.
        if running.is_none() && monitor.due() && connection.commands.is_empty() {
            if !monitor
                .poll(history, &mut auth, &mut catalog, &connection.events)
                .await
            {
                break;
            }
            continue;
        }
        if running.is_none() && !queue.state.paused && !queue.state.queued.is_empty() {
            // A retry of the same key must not receive progress from its old worker.
            while progress_rx.try_recv().is_ok() {}
            queue
                .start_next(history, &mut auth, &catalog, &downloads, &progress, running)
                .await;
            if !queue.send(running, &connection.events).await {
                break;
            }
        }
        let command = tokio::select! {
            biased;
            command = connection.commands.recv() => {
                let Ok(command) = command else { break };
                command
            }
            update = progress_rx.recv() => {
                if let Ok(update) = update {
                    let (key, event) = match update {
                        DownloadUpdate::Record(record) => (record.key, CoreEvent::DownloadUpdated(record)),
                        DownloadUpdate::Metrics { key, metrics } => (key, CoreEvent::DownloadMetrics { key, metrics }),
                    };
                    if running.as_ref().is_some_and(|job| job.entry.book.key == key)
                        && connection.events.send(event).await.is_err()
                    { break; }
                }
                continue;
            }
            mut result = transfers::wait(running) => {
                let Some(job) = running.take() else { break };
                let resumable = job.retain_on_shutdown && match &result {
                    Ok(Err(error)) => error.is_resumable(),
                    Err(_) => true,
                    _ => false,
                };
                if resumable {
                    let mut entry = job.entry.clone();
                    entry.interrupted = true;
                    queue.state.queued.insert(0, entry);
                    queue.state.paused = true;
                } else if matches!(result, Ok(Err(_)))
                    && let Err(error) = downloads.discard(&job.entry).await
                {
                    result = Ok(Err(error));
                    queue.state.paused = true;
                }
                if !queue.complete(result, job.entry.book, &downloads, &mut auth, &mut catalog, &connection.events).await
                { break; }
                if queue.owner.is_some() { queue.persist(history, running).await; }
                if !queue.send(running, &connection.events).await
                    || !load_history(history, &connection.events).await
                { break; }
                monitor.sync(history, &auth, &catalog, true).await;
                if !monitor.send(&connection.events).await { break; }
                continue;
            }
            _ = tokio::time::sleep_until(monitor.next), if monitor.state.monitoring && running.is_none() => {
                if !monitor.poll(history, &mut auth, &mut catalog, &connection.events).await { break; }
                continue;
            }
        };
        // Keep the Telegram connection under one network workflow at a time.
        // In particular, CancelDownload must not queue behind a 60-second scan.
        if running.is_some()
            && (matches!(
                &command,
                CoreCommand::RefreshChannels | CoreCommand::ScanNextPage
            ) || matches!(&command, CoreCommand::SelectChannel(Some(id)) if catalog.selected != Some(*id)))
        {
            catalog.reject(CatalogError::Busy);
            if !send_catalog(&catalog, &connection.events).await {
                break;
            }
            continue;
        }
        let recommendation_action = matches!(
            &command,
            CoreCommand::SetMonitoring { .. }
                | CoreCommand::ReloadRecommendations
                | CoreCommand::ChangeRecommendations { .. }
                | CoreCommand::DownloadRecommendations { .. }
        );
        match command {
            CoreCommand::SetMonitoring { account, enabled } => {
                if auth.account_id() != Some(account) || monitor.state.account != Some(account) {
                    monitor.state.error = Some(RecommendationError::Unauthorized);
                } else {
                    monitor.set_enabled(history, enabled, &auth, &catalog).await;
                }
                if !monitor.send(&connection.events).await {
                    break;
                }
            }
            CoreCommand::ReloadRecommendations => {
                monitor.sync(history, &auth, &catalog, true).await;
                if !monitor.send(&connection.events).await {
                    break;
                }
            }
            CoreCommand::ChangeRecommendations {
                account,
                keys,
                status,
            } => {
                let result = if auth.account_id() != Some(account)
                    || monitor.state.account != Some(account)
                {
                    Err(RecommendationError::Unauthorized)
                } else if keys.is_empty() || keys.len() > crate::MAX_PENDING_DOWNLOADS {
                    Err(RecommendationError::Unavailable)
                } else {
                    history
                        .set_recommendation_status(account, &keys, status)
                        .await
                };
                if let Err(error) = result {
                    monitor.state.error = Some(error);
                } else {
                    monitor.sync(history, &auth, &catalog, true).await;
                }
                if !monitor.send(&connection.events).await {
                    break;
                }
            }
            CoreCommand::DownloadRecommendations { account, keys } => {
                if auth.account_id() != Some(account) || monitor.state.account != Some(account) {
                    monitor.state.error = Some(RecommendationError::Unauthorized);
                } else if keys.len() > crate::MAX_PENDING_DOWNLOADS {
                    monitor.state.error = Some(RecommendationError::Download(
                        crate::DownloadError::QueueFull,
                    ));
                } else {
                    let books: Vec<_> = keys
                        .iter()
                        .filter_map(|key| {
                            monitor
                                .state
                                .items
                                .iter()
                                .find(|item| {
                                    item.book.key == *key
                                        && item.status != RecommendationStatus::Hidden
                                })
                                .map(|item| item.book.clone())
                        })
                        .collect();
                    if books.len() != keys.len()
                        || keys.is_empty()
                        || books.iter().any(|book| {
                            !catalog
                                .channels
                                .iter()
                                .any(|channel| channel.id == book.key.channel_id)
                        })
                    {
                        monitor.state.error = Some(RecommendationError::Unavailable);
                    } else {
                        let snapshot = CatalogState {
                            books,
                            ..catalog.clone()
                        };
                        if !enqueue_downloads(
                            history,
                            &auth,
                            &snapshot,
                            &mut queue,
                            running,
                            &connection.events,
                            (keys.clone(), settings.download_directory.clone(), false),
                        )
                        .await
                        {
                            break;
                        }
                        if let Some(error) = queue.state.error {
                            monitor.state.error = Some(RecommendationError::Download(error));
                        } else {
                            let admitted: Vec<_> = keys
                                .into_iter()
                                .filter(|key| {
                                    queue.state.queued.iter().any(|job| job.book.key == *key)
                                        || running
                                            .as_ref()
                                            .is_some_and(|job| job.entry.book.key == *key)
                                })
                                .collect();
                            match history
                                .set_recommendation_status(
                                    account,
                                    &admitted,
                                    RecommendationStatus::Read,
                                )
                                .await
                            {
                                Ok(()) => {
                                    monitor.sync(history, &auth, &catalog, true).await;
                                }
                                Err(error) => monitor.state.error = Some(error),
                            }
                        }
                    }
                }
                if !monitor.send(&connection.events).await {
                    break;
                }
            }
            CoreCommand::SaveSettings(draft) => {
                let result = if running.is_some() {
                    Err(SettingsError::Busy)
                } else {
                    history.save_settings(&draft).await
                };
                let mut formats_changed = false;
                if let Ok(accepted) = &result {
                    formats_changed = catalog.set_formats(accepted.formats);
                    settings = accepted.clone();
                }
                if connection
                    .events
                    .send(CoreEvent::SettingsSaved(result))
                    .await
                    .is_err()
                {
                    break;
                }
                if formats_changed {
                    if !send_catalog(&catalog, &connection.events).await {
                        break;
                    }
                    if catalog.can_scan()
                        && !scan_next_page(&mut auth, &mut catalog, &connection.events).await
                    {
                        break;
                    }
                }
            }
            CoreCommand::RefreshHistory => {
                if !load_history(history, &connection.events).await {
                    break;
                }
                if !restore_queue(history, &auth, &mut queue, running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::Authenticate(action) => {
                if running.is_some() || !queue.state.queued.is_empty() {
                    let mut state = auth.state();
                    state.error = Some(crate::AuthError::UnexpectedState);
                    if connection
                        .events
                        .send(CoreEvent::Authentication(state))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                if !authenticate(&mut auth, action, &connection.events).await {
                    break;
                }
                sources::sync(history, &auth, &mut catalog, false).await;
                if !catalog_after_auth(&mut auth, &mut catalog, &connection.events).await {
                    break;
                }
                if !restore_queue(history, &auth, &mut queue, running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::RefreshChannels => {
                sources::sync(history, &auth, &mut catalog, true).await;
                if !refresh_channels(&mut auth, &mut catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::ReloadSources => {
                sources::sync(history, &auth, &mut catalog, true).await;
                if !send_catalog(&catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::SetChannelSource { id, enabled } => {
                let result = sources::set(history, &auth, &mut catalog, id, enabled).await;
                if !send_catalog(&catalog, &connection.events).await
                    || connection
                        .events
                        .send(CoreEvent::SourceSaved(result))
                        .await
                        .is_err()
                {
                    break;
                }
                if catalog.sources.channels.is_empty() && monitor.state.monitoring {
                    monitor.set_enabled(history, false, &auth, &catalog).await;
                    if !monitor.send(&connection.events).await {
                        break;
                    }
                }
            }
            CoreCommand::SelectChannel(id) => {
                if !select_channel(&mut auth, &mut catalog, id, &connection.events).await {
                    break;
                }
            }
            CoreCommand::ScanNextPage => {
                if !scan_next_page(&mut auth, &mut catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::DownloadBooks { keys, directory } => {
                if !enqueue_downloads(
                    history,
                    &auth,
                    &catalog,
                    &mut queue,
                    running,
                    &connection.events,
                    (keys, directory, false),
                )
                .await
                {
                    break;
                }
            }
            CoreCommand::DownloadToDefault(keys) => {
                if !enqueue_downloads(
                    history,
                    &auth,
                    &catalog,
                    &mut queue,
                    running,
                    &connection.events,
                    (keys, settings.download_directory.clone(), false),
                )
                .await
                {
                    break;
                }
            }
            CoreCommand::RedownloadBook { key, directory } => {
                if !enqueue_downloads(
                    history,
                    &auth,
                    &catalog,
                    &mut queue,
                    running,
                    &connection.events,
                    (
                        vec![key],
                        directory.unwrap_or_else(|| settings.download_directory.clone()),
                        true,
                    ),
                )
                .await
                {
                    break;
                }
            }
            CoreCommand::CancelDownload(key) => {
                queue.cancel(key, history, &downloads, running).await;
                if !queue.send(running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::SetQueuePaused(paused) => {
                if !restore_queue(history, &auth, &mut queue, running, &connection.events).await {
                    break;
                }
                if queue.owner.is_none() {
                    if auth.state().step != AuthStep::Authorized {
                        queue.reject(crate::DownloadError::Unauthorized);
                        if !queue.send(running, &connection.events).await {
                            break;
                        }
                    }
                    continue;
                }
                queue.set_paused(paused, &auth);
                if !queue.send(running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::Shutdown => break,
        }
        if recommendation_action
            && connection
                .events
                .send(CoreEvent::RecommendationActionFinished)
                .await
                .is_err()
        {
            break;
        }
        if auth.state().step != AuthStep::Authorized && !queue.state.queued.is_empty() {
            queue.forget_account();
            queue.reject(crate::DownloadError::Unauthorized);
            if !queue.send(running, &connection.events).await {
                break;
            }
        }
    }
}

async fn enqueue_downloads(
    history: &(impl DownloadHistory + QueueStore),
    auth: &AuthController<impl Authentication>,
    catalog: &CatalogState,
    queue: &mut transfers::DownloadQueue,
    running: &Option<transfers::RunningDownload>,
    events: &Sender<CoreEvent>,
    (keys, directory, redownload): (Vec<DownloadKey>, PathBuf, bool),
) -> bool {
    if !restore_queue(history, auth, queue, running, events).await {
        return false;
    }
    if queue.owner.is_none() {
        if auth.state().step != AuthStep::Authorized {
            queue.reject(crate::DownloadError::Unauthorized);
            return queue.send(running, events).await;
        }
        // restore_queue already reported its failure. Do not emit a second
        // acknowledgement that could be mistaken for the next command's reply.
        return true;
    }
    match history.list_downloads().await {
        Ok(records) => {
            let mut next = queue.clone();
            if redownload {
                next.enqueue_with_policy(keys, directory, auth, catalog, running, (&records, true));
            } else {
                next.enqueue(keys, directory, auth, catalog, running, &records);
            }
            if next.state.queued == queue.state.queued {
                *queue = next;
            } else {
                queue.commit(next, history, running).await;
            }
        }
        Err(_) => {
            queue.reject(crate::DownloadError::Storage);
            queue.state.paused = true;
        }
    }
    queue.send(running, events).await
}

async fn restore_queue(
    store: &(impl DownloadHistory + QueueStore),
    auth: &AuthController<impl Authentication>,
    queue: &mut transfers::DownloadQueue,
    running: &Option<transfers::RunningDownload>,
    events: &Sender<CoreEvent>,
) -> bool {
    if auth.state().step != AuthStep::Authorized {
        queue.forget_account();
        return true;
    }
    match queue.restore(store, auth).await {
        Ok(false) => true,
        Ok(true) => queue.send(running, events).await,
        Err(error) => {
            queue.reject(error);
            queue.state.paused = true;
            queue.send(running, events).await
        }
    }
}

async fn send_catalog(state: &CatalogState, events: &Sender<CoreEvent>) -> bool {
    events.send(CoreEvent::Catalog(state.clone())).await.is_ok()
}

async fn catalog_after_auth(
    auth: &mut AuthController<impl Authentication + ChannelCatalog>,
    catalog: &mut CatalogState,
    events: &Sender<CoreEvent>,
) -> bool {
    let state = auth.state();
    if state.step == AuthStep::Authorized && state.error.is_none() {
        refresh_channels(auth, catalog, events).await
    } else if state.step != AuthStep::Authorized && catalog.authenticated {
        catalog.clear_authorization(None);
        send_catalog(catalog, events).await
    } else {
        true
    }
}

async fn catalog_failure(
    auth: &mut AuthController<impl Authentication + ChannelCatalog>,
    catalog: &mut CatalogState,
    error: CatalogError,
    events: &Sender<CoreEvent>,
) -> bool {
    if let CatalogError::FloodWait(seconds) = error {
        auth.record_flood_wait(seconds);
    }
    if error == CatalogError::Unauthorized {
        catalog.clear_authorization(Some(error));
        if !send_catalog(catalog, events).await {
            return false;
        }
        // Reuse the authentication path for revocation/key reset. Never keep
        // another account's channel metadata or retry discovery in a loop.
        if !authenticate(auth, AuthAction::Restore, events).await {
            return false;
        }
        catalog.authenticated = auth.state().step == AuthStep::Authorized;
    } else {
        catalog.reject(error);
    }
    send_catalog(catalog, events).await
}

async fn refresh_channels(
    auth: &mut AuthController<impl Authentication + ChannelCatalog>,
    catalog: &mut CatalogState,
    events: &Sender<CoreEvent>,
) -> bool {
    if auth.state().step != AuthStep::Authorized {
        catalog.clear_authorization(Some(CatalogError::Unauthorized));
        return send_catalog(catalog, events).await;
    }
    catalog.authenticated = true;
    if let Some(seconds) = auth.remaining_wait() {
        catalog.reject(CatalogError::FloodWait(seconds));
        return send_catalog(catalog, events).await;
    }
    catalog.operation = Some(CatalogOperation::ListChannels);
    catalog.channels_loaded = false;
    catalog.error = None;
    if !send_catalog(catalog, events).await {
        return false;
    }
    match auth.backend_mut().list_channels().await {
        Ok(channels) => {
            catalog.replace_channels(channels);
            catalog.operation = None;
            send_catalog(catalog, events).await
        }
        Err(error) => catalog_failure(auth, catalog, error, events).await,
    }
}

/// A new source loads its first page as one actor operation, not a second UI
/// command. Publishing ScanHistory first keeps selection busy until it finishes.
async fn select_channel(
    auth: &mut AuthController<impl Authentication + ChannelCatalog>,
    catalog: &mut CatalogState,
    id: Option<i64>,
    events: &Sender<CoreEvent>,
) -> bool {
    let previous = catalog.selected;
    let result = if auth.state().step == AuthStep::Authorized {
        catalog.select(id)
    } else {
        Err(CatalogError::Unauthorized)
    };
    if let Err(error) = result {
        catalog.reject(error);
        return send_catalog(catalog, events).await;
    }
    if previous != catalog.selected && catalog.can_scan() {
        scan_next_page(auth, catalog, events).await
    } else {
        send_catalog(catalog, events).await
    }
}

async fn scan_next_page(
    auth: &mut AuthController<impl Authentication + ChannelCatalog>,
    catalog: &mut CatalogState,
    events: &Sender<CoreEvent>,
) -> bool {
    if auth.state().step != AuthStep::Authorized {
        catalog.clear_authorization(Some(CatalogError::Unauthorized));
        return send_catalog(catalog, events).await;
    }
    if catalog
        .selected_channel()
        .is_some_and(|channel| channel.protected)
    {
        catalog.reject(CatalogError::ProtectedContent);
        return send_catalog(catalog, events).await;
    }
    if !catalog.can_scan() {
        if catalog.selected.is_none() {
            catalog.reject(CatalogError::Unavailable);
        }
        return send_catalog(catalog, events).await;
    }
    if let Some(seconds) = auth.remaining_wait() {
        catalog.reject(CatalogError::FloodWait(seconds));
        return send_catalog(catalog, events).await;
    }
    let Some(id) = catalog.selected else {
        return send_catalog(catalog, events).await;
    };
    catalog.operation = Some(CatalogOperation::ScanHistory);
    catalog.error = None;
    if !send_catalog(catalog, events).await {
        return false;
    }
    let result = auth.backend_mut().scan_books(id, catalog.next_offset).await;
    catalog.operation = None;
    match result.and_then(|page| catalog.append_page(page)) {
        Ok(()) => send_catalog(catalog, events).await,
        Err(error) => catalog_failure(auth, catalog, error, events).await,
    }
}

#[cfg(test)]
#[path = "catalog_service_tests.rs"]
mod catalog_tests;

#[cfg(test)]
#[path = "transfer_service_tests.rs"]
mod transfer_tests;

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, io};

    use super::*;
    use crate::downloads::UnusedDownloads;

    struct EmptyAuth;

    impl BookSource for EmptyAuth {}

    impl Authentication for EmptyAuth {
        async fn execute(&mut self, _: AuthAction) -> Result<crate::AuthStep, crate::AuthFailure> {
            Ok(crate::AuthStep::Credentials)
        }
    }

    impl ChannelCatalog for EmptyAuth {
        async fn list_channels(&mut self) -> Result<Vec<crate::ChannelSummary>, CatalogError> {
            Ok(Vec::new())
        }
        async fn scan_books(
            &mut self,
            _: i64,
            _: Option<i32>,
        ) -> Result<crate::BookPage, CatalogError> {
            Err(CatalogError::Unavailable)
        }
    }

    struct EmptyHistory;
    impl SettingsStore for EmptyHistory {}
    impl SourceStore for EmptyHistory {}
    impl RecommendationStore for EmptyHistory {}

    impl QueueStore for EmptyHistory {
        async fn load_queue(
            &self,
            _: i64,
        ) -> Result<Vec<crate::QueuedDownload>, crate::DownloadError> {
            Ok(Vec::new())
        }
        async fn save_queue(
            &self,
            _: i64,
            _: &[crate::QueuedDownload],
        ) -> Result<(), crate::DownloadError> {
            Ok(())
        }
    }

    impl DownloadHistory for EmptyHistory {
        type Error = Infallible;

        async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
            Ok(Vec::new())
        }
    }

    struct BrokenHistory;
    impl SettingsStore for BrokenHistory {}
    impl SourceStore for BrokenHistory {}
    impl RecommendationStore for BrokenHistory {}

    impl QueueStore for BrokenHistory {
        async fn load_queue(
            &self,
            _: i64,
        ) -> Result<Vec<crate::QueuedDownload>, crate::DownloadError> {
            Err(crate::DownloadError::Storage)
        }
        async fn save_queue(
            &self,
            _: i64,
            _: &[crate::QueuedDownload],
        ) -> Result<(), crate::DownloadError> {
            Err(crate::DownloadError::Storage)
        }
    }

    impl DownloadHistory for BrokenHistory {
        type Error = io::Error;

        async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
            Err(io::Error::other("private database details"))
        }
    }

    fn info() -> AppInfo {
        AppInfo {
            settings: AppSettings::new("books".into()),
            data_directory: "data".into(),
            download_directory: "books".into(),
        }
    }

    #[tokio::test]
    async fn commands_produce_ordered_events_and_shutdown() -> Result<(), Box<dyn Error>> {
        let (ui, backend) = connect();
        let task = tokio::spawn(async {
            run_service(&EmptyHistory, EmptyAuth, UnusedDownloads, info(), backend).await
        });
        assert_eq!(ui.events.recv().await?, CoreEvent::Ready(info()));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(Vec::new())
        );
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::Authentication(AuthState { busy: true, .. })
        ));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::Authentication(AuthState::default())
        );
        ui.commands.send(CoreCommand::RefreshHistory).await?;
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(Vec::new())
        );
        ui.commands.send(CoreCommand::Shutdown).await?;
        task.await?;
        assert!(ui.events.recv().await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn adapter_errors_are_not_exposed_to_ui() -> Result<(), Box<dyn Error>> {
        let (ui, backend) = connect();
        let task = tokio::spawn(async {
            run_service(&BrokenHistory, EmptyAuth, UnusedDownloads, info(), backend).await
        });
        assert_eq!(ui.events.recv().await?, CoreEvent::Ready(info()));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::Failed(ServiceFailure::Storage)
        );
        drop(ui);
        task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn disconnected_ui_stops_the_service() -> Result<(), Box<dyn Error>> {
        let (ui, backend) = connect();
        drop(ui);
        run_service(&EmptyHistory, EmptyAuth, UnusedDownloads, info(), backend).await;
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_cancels_an_in_flight_authentication_without_waiting_for_rpc()
    -> Result<(), Box<dyn Error>> {
        struct HangingAuth;
        impl BookSource for HangingAuth {}
        impl Authentication for HangingAuth {
            async fn execute(
                &mut self,
                _: AuthAction,
            ) -> Result<crate::AuthStep, crate::AuthFailure> {
                std::future::pending().await
            }
        }
        impl ChannelCatalog for HangingAuth {
            async fn list_channels(&mut self) -> Result<Vec<crate::ChannelSummary>, CatalogError> {
                Ok(Vec::new())
            }
            async fn scan_books(
                &mut self,
                _: i64,
                _: Option<i32>,
            ) -> Result<crate::BookPage, CatalogError> {
                Err(CatalogError::Unavailable)
            }
        }
        let (ui, backend) = connect();
        let task = tokio::spawn(async {
            run_service(&EmptyHistory, HangingAuth, UnusedDownloads, info(), backend).await
        });
        assert!(matches!(ui.events.recv().await?, CoreEvent::Ready(_)));
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(_)
        ));
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::Authentication(AuthState { busy: true, .. })
        ));
        ui.shutdown.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(100), task).await??;
        Ok(())
    }
}
