use super::*;
use crate::{BookAttachment, BookFormats, DiscoveredBook, DownloadKey};

fn page(newest: i32, next: Option<i32>, messages: &[i32]) -> MonitorPage {
    MonitorPage {
        newest: Some(newest),
        next_offset: next,
        scanned: if next.is_some() { 100 } else { 10 },
        skipped_protected: 0,
        books: messages
            .iter()
            .map(|&id| DiscoveredBook {
                book: BookAttachment {
                    key: DownloadKey {
                        channel_id: -1,
                        message_id: id,
                        document_id: i64::from(id),
                    },
                    file_name: format!("Book-{id}.epub"),
                    format: BookFormat::Epub,
                    size_bytes: Some(1),
                },
                message_date: 123,
            })
            .collect(),
    }
}

#[test]
fn first_enable_baselines_all_messages_including_empty_channels_without_recommending_history() {
    let result = process_page(
        -1,
        "Books",
        BookFormats::default(),
        None,
        page(100, Some(1), &[99]),
    );
    let Ok((cursor, items)) = result else {
        panic!("valid baseline");
    };
    assert_eq!(
        cursor,
        MonitorCursor {
            after: 100,
            before: None,
            target: None
        }
    );
    assert!(items.is_empty());
    let empty = MonitorPage {
        newest: None,
        next_offset: None,
        books: Vec::new(),
        scanned: 0,
        skipped_protected: 0,
    };
    assert_eq!(
        process_page(-1, "Books", BookFormats::default(), None, empty),
        Ok((
            MonitorCursor {
                after: 0,
                before: None,
                target: None
            },
            Vec::new()
        ))
    );
}

#[test]
fn backlog_is_walked_without_skipping_middle_pages_and_freezes_its_target() {
    let initial = MonitorCursor {
        after: 100,
        before: None,
        target: None,
    };
    let Ok((first, books)) = process_page(
        -1,
        "Books",
        BookFormats::default(),
        Some(initial),
        page(350, Some(251), &[350, 260]),
    ) else {
        panic!("first page");
    };
    assert_eq!(
        first,
        MonitorCursor {
            after: 100,
            before: Some(251),
            target: Some(350)
        }
    );
    assert_eq!(books.len(), 2);
    let Ok((second, books)) = process_page(
        -1,
        "Books",
        BookFormats::default(),
        Some(first),
        page(250, Some(151), &[240, 160]),
    ) else {
        panic!("second page");
    };
    assert_eq!(second.target, Some(350));
    assert_eq!(books.len(), 2);
    let Ok((finished, books)) = process_page(
        -1,
        "Books",
        BookFormats::default(),
        Some(second),
        page(150, None, &[149, 101, 100, 99]),
    ) else {
        panic!("last page");
    };
    assert_eq!(
        finished,
        MonitorCursor {
            after: 350,
            before: None,
            target: None
        }
    );
    assert_eq!(books.len(), 2);
    // A message arriving during the frozen walk is picked up in the next walk.
    let Ok((next, books)) = process_page(
        -1,
        "Books",
        BookFormats::default(),
        Some(finished),
        page(360, None, &[360, 350]),
    ) else {
        panic!("next walk");
    };
    assert_eq!(next.after, 360);
    assert_eq!(books.len(), 1);
}

#[test]
fn empty_and_filtered_pages_advance_but_invalid_output_never_advances() {
    let cursor = MonitorCursor {
        after: 10,
        before: None,
        target: None,
    };
    let Some(pdf_only) = BookFormats::from_mask(1) else {
        panic!("pdf mask");
    };
    let Ok((next, items)) = process_page(
        -1,
        "Books",
        pdf_only,
        Some(cursor),
        page(20, None, &[20, 19]),
    ) else {
        panic!("filtered page");
    };
    assert_eq!(next.after, 20);
    assert!(items.is_empty());
    let Ok((next, _)) = process_page(-1, "Books", pdf_only, Some(next), page(30, None, &[])) else {
        panic!("no attachments");
    };
    assert_eq!(next.after, 30);
    let mut invalid = page(50, Some(51), &[50]);
    assert!(process_page(-1, "Books", pdf_only, Some(next), invalid.clone()).is_err());
    invalid.next_offset = None;
    invalid.books[0].book.key.channel_id = -2;
    assert!(process_page(-1, "Books", pdf_only, Some(next), invalid).is_err());
    let pending = MonitorCursor {
        after: 10,
        before: Some(40),
        target: Some(50),
    };
    assert!(
        process_page(
            -1,
            "Books",
            pdf_only,
            Some(pending),
            page(40, Some(40), &[])
        )
        .is_err()
    );
}

struct Account;
impl Authentication for Account {
    fn account_id(&self) -> Option<i64> {
        Some(1)
    }
    async fn execute(&mut self, _: crate::AuthAction) -> Result<AuthStep, crate::AuthFailure> {
        Ok(AuthStep::Authorized)
    }
}
impl ChannelCatalog for Account {
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
    async fn scan_updates(&mut self, _: i64, _: Option<i32>) -> Result<MonitorPage, CatalogError> {
        Err(CatalogError::FloodWait(120))
    }
}
struct Store;
impl RecommendationStore for Store {
    async fn save_monitoring_enabled(&self, _: i64, _: bool) -> Result<(), Error> {
        Ok(())
    }
}

#[tokio::test]
async fn failed_preference_reads_or_writes_do_not_start_background_requests() {
    struct WriteFailure;
    impl RecommendationStore for WriteFailure {}
    struct ReadFailure;
    impl RecommendationStore for ReadFailure {
        async fn load_monitoring_enabled(&self, _: i64) -> Result<bool, Error> {
            Err(Error::Storage)
        }
    }
    let mut auth = AuthController::new(Account);
    auth.complete(Ok(AuthStep::Authorized));
    let source = crate::ChannelSummary {
        id: -1,
        title: "Books".into(),
        username: None,
        protected: false,
    };
    let catalog = CatalogState {
        authenticated: true,
        channels: vec![source.clone()],
        sources: crate::SavedSources {
            account: Some(1),
            loaded: true,
            channels: vec![source],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut monitor = Monitor::default();
    monitor.sync(&ReadFailure, &auth, &catalog, false).await;
    assert!(!monitor.state.loaded && !monitor.state.monitoring && !monitor.due());
    assert_eq!(monitor.state.error, Some(Error::Storage));
    monitor.sync(&WriteFailure, &auth, &catalog, true).await;
    assert!(monitor.state.loaded);
    monitor
        .set_enabled(&WriteFailure, true, &auth, &catalog)
        .await;
    assert!(!monitor.state.monitoring && !monitor.due());
    assert_eq!(monitor.state.error, Some(Error::Storage));
}

#[tokio::test]
async fn flood_wait_stops_monitoring_and_cannot_be_reenabled_before_shared_cooldown() {
    let mut auth = AuthController::new(Account);
    auth.complete(Ok(AuthStep::Authorized));
    let mut catalog = CatalogState::default();
    let source = crate::ChannelSummary {
        id: -1,
        title: "Books".into(),
        username: None,
        protected: false,
    };
    catalog.channels.push(source.clone());
    catalog.sources = crate::SavedSources {
        account: Some(1),
        loaded: true,
        channels: vec![source],
        ..Default::default()
    };
    let mut monitor = Monitor::default();
    monitor.sync(&Store, &auth, &catalog, false).await;
    monitor.enable(true, &auth, &catalog);
    assert!(monitor.state.monitoring);
    let (events, _receiver) = async_channel::unbounded();
    assert!(monitor.poll(&Store, &mut auth, &mut catalog, &events).await);
    assert!(!monitor.state.monitoring);
    assert_eq!(
        monitor.state.error,
        Some(Error::Telegram(CatalogError::FloodWait(120)))
    );
    monitor.enable(true, &auth, &catalog);
    assert!(!monitor.state.monitoring);
    assert!(auth.remaining_wait().is_some());
    monitor.state.items.push(Recommendation {
        book: page(20, None, &[20]).books.remove(0).book,
        source_title: "Private".into(),
        message_date: 1,
        status: RecommendationStatus::Unread,
    });
    auth.complete(Ok(AuthStep::Phone));
    assert!(monitor.sync(&Store, &auth, &catalog, false).await);
    assert_eq!(monitor.state, RecommendationState::default());
}

#[tokio::test]
async fn revoked_session_never_republishes_the_old_inbox_after_a_different_account_restores() {
    struct SwitchingAccount(i64);
    impl Authentication for SwitchingAccount {
        fn account_id(&self) -> Option<i64> {
            Some(self.0)
        }
        async fn execute(&mut self, _: crate::AuthAction) -> Result<AuthStep, crate::AuthFailure> {
            self.0 = 2;
            Ok(AuthStep::Authorized)
        }
    }
    impl ChannelCatalog for SwitchingAccount {
        async fn list_channels(&mut self) -> Result<Vec<crate::ChannelSummary>, CatalogError> {
            Ok(Vec::new())
        }
        async fn scan_books(
            &mut self,
            _: i64,
            _: Option<i32>,
        ) -> Result<crate::BookPage, CatalogError> {
            Err(CatalogError::Unauthorized)
        }
        async fn scan_updates(
            &mut self,
            _: i64,
            _: Option<i32>,
        ) -> Result<MonitorPage, CatalogError> {
            Err(CatalogError::Unauthorized)
        }
    }
    let mut auth = AuthController::new(SwitchingAccount(1));
    auth.complete(Ok(AuthStep::Authorized));
    let source = crate::ChannelSummary {
        id: -1,
        title: "Private".into(),
        username: None,
        protected: false,
    };
    let mut catalog = CatalogState {
        authenticated: true,
        channels: vec![source.clone()],
        sources: crate::SavedSources {
            account: Some(1),
            loaded: true,
            channels: vec![source],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut monitor = Monitor::default();
    monitor.sync(&Store, &auth, &catalog, false).await;
    monitor.state.items.push(Recommendation {
        book: page(20, None, &[20]).books.remove(0).book,
        source_title: "Private".into(),
        message_date: 1,
        status: RecommendationStatus::Unread,
    });
    monitor.enable(true, &auth, &catalog);
    let (events, receiver) = async_channel::unbounded();
    assert!(monitor.poll(&Store, &mut auth, &mut catalog, &events).await);
    assert_eq!(monitor.state.account, Some(2));
    assert!(monitor.state.items.is_empty());
    assert!(!monitor.state.monitoring);
    let mut last = None;
    while let Ok(event) = receiver.try_recv() {
        if let CoreEvent::Recommendations(state) = event {
            last = Some(state);
        }
    }
    assert_eq!(last, Some(monitor.state));
}
