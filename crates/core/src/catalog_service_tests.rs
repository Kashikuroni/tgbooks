use std::{collections::VecDeque, convert::Infallible, time::Duration};

use crate::{
    AuthFailure, BookAttachment, BookFormat, BookPage, ChannelSummary, DownloadKey,
    downloads::UnusedDownloads,
};

use super::*;

#[derive(Debug, Eq, PartialEq)]
enum Call {
    Authenticate,
    List,
    Scan(i64, Option<i32>),
}

#[derive(Default)]
struct FakeTelegram {
    steps: VecDeque<AuthStep>,
    lists: VecDeque<Result<Vec<ChannelSummary>, CatalogError>>,
    pages: VecDeque<Result<BookPage, CatalogError>>,
    calls: Vec<Call>,
    hang_scan: bool,
}

impl BookSource for FakeTelegram {}

impl Authentication for FakeTelegram {
    async fn execute(&mut self, _: AuthAction) -> Result<AuthStep, AuthFailure> {
        self.calls.push(Call::Authenticate);
        Ok(self.steps.pop_front().unwrap_or(AuthStep::Phone))
    }
}

impl ChannelCatalog for FakeTelegram {
    async fn list_channels(&mut self) -> Result<Vec<ChannelSummary>, CatalogError> {
        self.calls.push(Call::List);
        self.lists.pop_front().unwrap_or(Ok(channels()))
    }

    async fn scan_books(&mut self, id: i64, before: Option<i32>) -> Result<BookPage, CatalogError> {
        self.calls.push(Call::Scan(id, before));
        if self.hang_scan {
            return std::future::pending().await;
        }
        self.pages
            .pop_front()
            .unwrap_or(Err(CatalogError::Unavailable))
    }
}

struct EmptyHistory;

impl DownloadHistory for EmptyHistory {
    type Error = Infallible;
    async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
        Ok(Vec::new())
    }
}

const CHANNEL: i64 = -1000000000001;

fn channels() -> Vec<ChannelSummary> {
    vec![ChannelSummary {
        id: CHANNEL,
        title: "Книги".into(),
        username: None,
        protected: false,
    }]
}

fn page(message_id: i32, next_offset: Option<i32>) -> BookPage {
    BookPage {
        books: vec![BookAttachment {
            key: DownloadKey {
                channel_id: CHANNEL,
                message_id,
                document_id: 99,
            },
            file_name: "Book.epub".into(),
            size_bytes: None,
            format: BookFormat::Epub,
        }],
        scanned: 100,
        skipped_protected: 1,
        next_offset,
    }
}

fn authorized(backend: FakeTelegram) -> AuthController<FakeTelegram> {
    let mut auth = AuthController::new(backend);
    auth.complete(Ok(AuthStep::Authorized));
    auth
}

fn selected() -> CatalogState {
    CatalogState {
        authenticated: true,
        channels: channels(),
        selected: Some(CHANNEL),
        ..Default::default()
    }
}

#[tokio::test]
async fn restored_login_loads_channels_through_the_actor_before_commands()
-> Result<(), Box<dyn Error>> {
    let (ui, backend) = connect();
    let telegram = FakeTelegram {
        steps: [AuthStep::Authorized].into(),
        ..Default::default()
    };
    let task = tokio::spawn(async {
        run_service(
            &EmptyHistory,
            telegram,
            UnusedDownloads,
            AppInfo {
                data_directory: "data".into(),
                download_directory: "books".into(),
            },
            backend,
        )
        .await
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
    assert!(matches!(
        ui.events.recv().await?,
        CoreEvent::Authentication(AuthState {
            step: AuthStep::Authorized,
            busy: false,
            ..
        })
    ));
    assert!(matches!(
        ui.events.recv().await?,
        CoreEvent::Catalog(CatalogState {
            operation: Some(CatalogOperation::ListChannels),
            ..
        })
    ));
    let CoreEvent::Catalog(state) = ui.events.recv().await? else {
        panic!("catalog missing")
    };
    assert!(state.authenticated);
    assert_eq!(state.channels, channels());
    assert!(!state.busy());
    ui.commands
        .send(CoreCommand::SelectChannel(Some(CHANNEL)))
        .await?;
    let CoreEvent::Catalog(state) = ui.events.recv().await? else {
        panic!("selection missing")
    };
    assert_eq!(state.selected, Some(CHANNEL));
    ui.commands.send(CoreCommand::Shutdown).await?;
    task.await?;
    Ok(())
}

#[tokio::test]
async fn no_catalog_rpc_before_login_or_for_protected_channels() {
    let (events, _receiver) = async_channel::unbounded();
    let mut auth = AuthController::new(FakeTelegram::default());
    let mut catalog = CatalogState::default();
    assert!(refresh_channels(&mut auth, &mut catalog, &events).await);
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert_eq!(catalog.error, Some(CatalogError::Unauthorized));
    assert!(auth.backend_mut().calls.is_empty());

    auth.complete(Ok(AuthStep::Authorized));
    catalog = selected();
    catalog.channels[0].protected = true;
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert_eq!(catalog.error, Some(CatalogError::ProtectedContent));
    assert!(auth.backend_mut().calls.is_empty());
}

#[tokio::test]
async fn failed_page_can_be_retried_and_exhaustion_never_reissues_rpc() {
    let (events, _receiver) = async_channel::unbounded();
    let mut auth = authorized(FakeTelegram {
        pages: [
            Ok(page(100, Some(90))),
            Err(CatalogError::Network),
            Ok(page(80, None)),
        ]
        .into(),
        ..Default::default()
    });
    let mut catalog = selected();
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    let first = catalog.clone();
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert_eq!(catalog.books, first.books);
    assert_eq!(catalog.next_offset, first.next_offset);
    assert_eq!(catalog.scanned_messages, 100);
    assert_eq!(catalog.error, Some(CatalogError::Network));
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert_eq!(catalog.books.len(), 2);
    assert_eq!(catalog.scanned_messages, 200);
    assert_eq!(catalog.error, None);
    assert!(!catalog.can_scan());
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert_eq!(
        auth.backend_mut().calls,
        vec![
            Call::Scan(CHANNEL, None),
            Call::Scan(CHANNEL, Some(90)),
            Call::Scan(CHANNEL, Some(90)),
        ]
    );
}

#[tokio::test]
async fn failed_channel_refresh_preserves_selection_books_and_cursor() {
    let (events, _receiver) = async_channel::unbounded();
    let mut auth = authorized(FakeTelegram {
        lists: [Err(CatalogError::Network), Ok(channels())].into(),
        ..Default::default()
    });
    let mut catalog = selected();
    assert_eq!(catalog.append_page(page(100, Some(90))), Ok(()));
    let before = catalog.clone();
    assert!(refresh_channels(&mut auth, &mut catalog, &events).await);
    assert_eq!(catalog.selected, before.selected);
    assert_eq!(catalog.books, before.books);
    assert_eq!(catalog.next_offset, before.next_offset);
    assert_eq!(catalog.error, Some(CatalogError::Network));
    assert!(refresh_channels(&mut auth, &mut catalog, &events).await);
    assert_eq!(catalog, before);
}

#[tokio::test]
async fn flood_wait_is_shared_by_listing_scanning_and_authentication() {
    let (events, _receiver) = async_channel::unbounded();
    let mut auth = authorized(FakeTelegram {
        pages: [Err(CatalogError::FloodWait(60))].into(),
        ..Default::default()
    });
    let mut catalog = selected();
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert!(refresh_channels(&mut auth, &mut catalog, &events).await);
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert!(auth.prepare(AuthAction::Restore).is_none());
    assert!(matches!(
        catalog.error,
        Some(CatalogError::FloodWait(1..=60))
    ));
    assert_eq!(auth.backend_mut().calls, vec![Call::Scan(CHANNEL, None)]);
}

#[tokio::test]
async fn revoked_authorization_clears_metadata_and_restores_without_discovery_loop() {
    let (events, _receiver) = async_channel::unbounded();
    let mut auth = authorized(FakeTelegram {
        steps: [AuthStep::Phone].into(),
        pages: [Err(CatalogError::Unauthorized)].into(),
        ..Default::default()
    });
    let mut catalog = selected();
    assert_eq!(catalog.append_page(page(100, Some(90))), Ok(()));
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert!(!catalog.authenticated);
    assert!(catalog.channels.is_empty());
    assert!(catalog.books.is_empty());
    assert_eq!(auth.state().step, AuthStep::Phone);
    assert_eq!(
        auth.backend_mut().calls,
        vec![Call::Scan(CHANNEL, Some(90)), Call::Authenticate]
    );
}

#[tokio::test]
async fn protection_changed_on_telegram_discards_previous_results() {
    let (events, _receiver) = async_channel::unbounded();
    let mut auth = authorized(FakeTelegram {
        pages: [Err(CatalogError::ProtectedContent)].into(),
        ..Default::default()
    });
    let mut catalog = selected();
    assert_eq!(catalog.append_page(page(100, Some(90))), Ok(()));
    assert!(scan_next_page(&mut auth, &mut catalog, &events).await);
    assert!(catalog.books.is_empty());
    assert!(
        catalog
            .selected_channel()
            .is_some_and(|channel| channel.protected)
    );
    assert!(!catalog.can_scan());
}

#[tokio::test]
async fn quit_cancels_an_in_flight_history_scan() -> Result<(), Box<dyn Error>> {
    let (ui, backend) = connect();
    let telegram = FakeTelegram {
        steps: [AuthStep::Authorized].into(),
        hang_scan: true,
        ..Default::default()
    };
    let task = tokio::spawn(async {
        run_service(
            &EmptyHistory,
            telegram,
            UnusedDownloads,
            AppInfo {
                data_directory: "data".into(),
                download_directory: "books".into(),
            },
            backend,
        )
        .await
    });
    for _ in 0..6 {
        ui.events.recv().await?;
    }
    ui.commands
        .send(CoreCommand::SelectChannel(Some(CHANNEL)))
        .await?;
    ui.events.recv().await?;
    ui.commands.send(CoreCommand::ScanNextPage).await?;
    assert!(matches!(
        ui.events.recv().await?,
        CoreEvent::Catalog(CatalogState {
            operation: Some(CatalogOperation::ScanHistory),
            ..
        })
    ));
    ui.shutdown.cancel();
    tokio::time::timeout(Duration::from_millis(100), task).await??;
    Ok(())
}
