use std::{error::Error, future::Future, path::PathBuf};

use async_channel::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

use crate::{
    AuthAction, AuthState, AuthStep, Authentication, BookSource, CatalogError, CatalogOperation,
    CatalogState, ChannelCatalog, DownloadEngine, DownloadKey, DownloadRecord, DownloadUpdate,
    TransferMetrics, TransferState, auth::AuthController,
};

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
    RefreshHistory,
    Authenticate(AuthAction),
    RefreshChannels,
    SelectChannel(Option<i64>),
    ScanNextPage,
    DownloadBooks {
        keys: Vec<DownloadKey>,
        directory: PathBuf,
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
    Ready(AppInfo),
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
    history: &impl DownloadHistory,
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
    history: &impl DownloadHistory,
    authentication: impl Authentication + ChannelCatalog + BookSource,
    downloads: impl DownloadEngine,
    info: AppInfo,
    connection: ServiceConnection,
    running: &mut Option<transfers::RunningDownload>,
) {
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
    let mut catalog = CatalogState::default();
    if !authenticate(&mut auth, AuthAction::Restore, &connection.events).await {
        return;
    }
    if !catalog_after_auth(&mut auth, &mut catalog, &connection.events).await {
        return;
    }

    let (progress, progress_rx) = async_channel::bounded::<DownloadUpdate>(16);
    let mut queue = transfers::DownloadQueue::default();
    loop {
        if running.is_none() && !queue.state.paused && !queue.state.queued.is_empty() {
            // A retry of the same key must not receive progress from its old worker.
            while progress_rx.try_recv().is_ok() {}
            queue.start_next(&mut auth, &catalog, &downloads, &progress, running);
            if !queue.send(running, &connection.events).await {
                break;
            }
        }
        let command = tokio::select! {
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
                    if running.as_ref().is_some_and(|job| job.book.key == key)
                        && connection.events.send(event).await.is_err()
                    { break; }
                }
                continue;
            }
            result = transfers::wait(running) => {
                let Some(job) = running.take() else { break };
                if !queue.complete(result, job.book, &downloads, &mut auth, &mut catalog, &connection.events).await
                    || !queue.send(running, &connection.events).await
                    || !load_history(history, &connection.events).await
                { break; }
                continue;
            }
        };
        // Keep the Telegram connection under one network workflow at a time.
        // In particular, CancelDownload must not queue behind a 60-second scan.
        if running.is_some()
            && matches!(
                &command,
                CoreCommand::RefreshChannels | CoreCommand::ScanNextPage
            )
        {
            catalog.reject(CatalogError::Busy);
            if !send_catalog(&catalog, &connection.events).await {
                break;
            }
            continue;
        }
        match command {
            CoreCommand::RefreshHistory => {
                if !load_history(history, &connection.events).await {
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
                if !catalog_after_auth(&mut auth, &mut catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::RefreshChannels => {
                if !refresh_channels(&mut auth, &mut catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::SelectChannel(id) => {
                let result = if auth.state().step == AuthStep::Authorized {
                    catalog.select(id)
                } else {
                    Err(CatalogError::Unauthorized)
                };
                if let Err(error) = result {
                    catalog.reject(error);
                }
                if !send_catalog(&catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::ScanNextPage => {
                if !scan_next_page(&mut auth, &mut catalog, &connection.events).await {
                    break;
                }
            }
            CoreCommand::DownloadBooks { keys, directory } => {
                match history.list_downloads().await {
                    Ok(records) => {
                        queue.enqueue(keys, directory, &auth, &catalog, running, &records)
                    }
                    Err(_) => queue.reject(crate::DownloadError::Storage),
                }
                if !queue.send(running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::CancelDownload(key) => {
                if let Some(job) = running.as_ref().filter(|job| job.book.key == key) {
                    job.cancel();
                } else {
                    queue.state.queued.retain(|job| job.book.key != key);
                }
                if !queue.send(running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::SetQueuePaused(paused) => {
                queue.set_paused(paused, &auth);
                if !queue.send(running, &connection.events).await {
                    break;
                }
            }
            CoreCommand::Shutdown => break,
        }
        if auth.state().step != AuthStep::Authorized && !queue.state.queued.is_empty() {
            queue.state.queued.clear();
            queue.state.paused = true;
            queue.reject(crate::DownloadError::Unauthorized);
            if !queue.send(running, &connection.events).await {
                break;
            }
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
        *catalog = CatalogState::default();
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
        *catalog = CatalogState {
            error: Some(error),
            ..Default::default()
        };
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
        *catalog = CatalogState {
            error: Some(CatalogError::Unauthorized),
            ..Default::default()
        };
        return send_catalog(catalog, events).await;
    }
    catalog.authenticated = true;
    if let Some(seconds) = auth.remaining_wait() {
        catalog.reject(CatalogError::FloodWait(seconds));
        return send_catalog(catalog, events).await;
    }
    catalog.operation = Some(CatalogOperation::ListChannels);
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

async fn scan_next_page(
    auth: &mut AuthController<impl Authentication + ChannelCatalog>,
    catalog: &mut CatalogState,
    events: &Sender<CoreEvent>,
) -> bool {
    if auth.state().step != AuthStep::Authorized {
        *catalog = CatalogState {
            error: Some(CatalogError::Unauthorized),
            ..Default::default()
        };
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

    impl DownloadHistory for EmptyHistory {
        type Error = Infallible;

        async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
            Ok(Vec::new())
        }
    }

    struct BrokenHistory;

    impl DownloadHistory for BrokenHistory {
        type Error = io::Error;

        async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
            Err(io::Error::other("private database details"))
        }
    }

    fn info() -> AppInfo {
        AppInfo {
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
