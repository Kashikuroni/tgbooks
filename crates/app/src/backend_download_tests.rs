//! An offline end-to-end workflow through the same actor, disk and SQLite as UI.

use std::time::Duration;

use tgbooks_core::{
    AuthAction, AuthFailure, AuthStep, Authentication, BookAttachment, BookChunks, BookFormat,
    BookFuture, BookPage, BookSource, CatalogError, ChannelCatalog, ChannelSummary, ChunkFuture,
    CoreCommand, DownloadHistory, DownloadKey, DownloadRecord, DownloadStatus, RemoteBook,
};

use super::*;

fn book() -> BookAttachment {
    BookAttachment {
        key: DownloadKey {
            channel_id: -1000000000001,
            message_id: 7,
            document_id: 8,
        },
        file_name: "Книга 世界.epub".into(),
        size_bytes: Some(8),
        format: BookFormat::Epub,
    }
}

struct FakeTelegram;

fn books() -> Vec<BookAttachment> {
    (0..3)
        .map(|index| {
            let mut attachment = book();
            attachment.key.message_id += index;
            attachment.key.document_id += i64::from(index);
            attachment.file_name = format!("Книга-{index}.epub");
            attachment
        })
        .collect()
}

impl Authentication for FakeTelegram {
    fn account_id(&self) -> Option<i64> {
        Some(1)
    }
    async fn execute(&mut self, _: AuthAction) -> Result<AuthStep, AuthFailure> {
        Ok(AuthStep::Authorized)
    }
}
impl ChannelCatalog for FakeTelegram {
    async fn list_channels(&mut self) -> Result<Vec<ChannelSummary>, CatalogError> {
        Ok(vec![ChannelSummary {
            id: book().key.channel_id,
            title: "Books".into(),
            username: None,
            protected: false,
        }])
    }
    async fn scan_books(&mut self, _: i64, _: Option<i32>) -> Result<BookPage, CatalogError> {
        Ok(BookPage {
            books: books(),
            scanned: 3,
            skipped_protected: 0,
            next_offset: None,
        })
    }
}
impl BookSource for FakeTelegram {
    fn open_book(&self, key: DownloadKey) -> BookFuture {
        Box::pin(async move {
            if key.message_id == book().key.message_id + 1 {
                return Err(tgbooks_core::DownloadError::Network);
            }
            let attachment = books()
                .into_iter()
                .find(|book| book.key == key)
                .ok_or(tgbooks_core::DownloadError::Unavailable)?;
            Ok(RemoteBook {
                book: attachment,
                source_title: Some("Books".into()),
                chunks: Box::new(OfflineChunks(0)),
            })
        })
    }
}

struct OfflineChunks(usize);
impl BookChunks for OfflineChunks {
    fn seek(&mut self, offset: u64) -> Result<(), tgbooks_core::DownloadError> {
        if offset > 8 || !offset.is_multiple_of(4) {
            return Err(tgbooks_core::DownloadError::InvalidFile);
        }
        self.0 = (offset / 4) as usize;
        Ok(())
    }

    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async {
            let bytes = match self.0 {
                0 => b"epub",
                1 => b"data",
                _ => return Ok(None),
            };
            self.0 += 1;
            // Exercise the actual throttled progress path, not just a final 100%.
            tokio::time::sleep(Duration::from_millis(650)).await;
            Ok(Some(bytes.to_vec()))
        })
    }
}

async fn event_until(
    events: &async_channel::Receiver<CoreEvent>,
    predicate: impl Fn(&CoreEvent) -> bool,
) -> Result<CoreEvent> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await?;
            if predicate(&event) {
                return Ok(event);
            }
        }
    })
    .await?
}

#[tokio::test]
async fn monitoring_accumulates_recommendations_and_downloads_only_after_explicit_consent()
-> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tgbooks_core::{
        DiscoveredBook, MonitorPage, RecommendationStatus as Status, RecommendationStore,
        SourceStore,
    };
    struct MonitoringTelegram(Arc<AtomicUsize>);
    impl Authentication for MonitoringTelegram {
        fn account_id(&self) -> Option<i64> {
            Some(1)
        }
        async fn execute(&mut self, _: AuthAction) -> std::result::Result<AuthStep, AuthFailure> {
            Ok(AuthStep::Authorized)
        }
    }
    impl ChannelCatalog for MonitoringTelegram {
        async fn list_channels(
            &mut self,
        ) -> std::result::Result<Vec<ChannelSummary>, CatalogError> {
            FakeTelegram.list_channels().await
        }
        async fn scan_books(
            &mut self,
            id: i64,
            before: Option<i32>,
        ) -> std::result::Result<BookPage, CatalogError> {
            FakeTelegram.scan_books(id, before).await
        }
        async fn scan_updates(
            &mut self,
            _: i64,
            _: Option<i32>,
        ) -> std::result::Result<MonitorPage, CatalogError> {
            let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
            Ok(MonitorPage {
                books: if first {
                    Vec::new()
                } else {
                    books()
                        .into_iter()
                        .filter(|book| book.key.message_id != 8)
                        .map(|book| DiscoveredBook {
                            book,
                            message_date: 1_794_000_000,
                        })
                        .collect()
                },
                newest: Some(if first { 6 } else { 9 }),
                next_offset: None,
                scanned: 3,
                skipped_protected: 0,
            })
        }
    }
    impl BookSource for MonitoringTelegram {
        fn open_book(&self, key: DownloadKey) -> BookFuture {
            FakeTelegram.open_book(key)
        }
    }
    let temporary = tempfile::tempdir()?;
    let paths = AppPaths::in_directory(temporary.path().join("application"))?;
    for attempt in 0..2 {
        let (storage, info) = initialize(paths.clone()).await?;
        if attempt == 0 {
            let channel = FakeTelegram
                .list_channels()
                .await
                .map_err(|_| anyhow::anyhow!("fake channel list failed"))?
                .remove(0);
            assert_eq!(storage.set_source(1, &channel, true).await, Ok(()));
        }
        let inspection = storage.clone();
        let polls = Arc::new(AtomicUsize::new(0));
        let source = MonitoringTelegram(polls.clone());
        let (ui, backend) = tgbooks_core::connect();
        let task = tokio::spawn(async move {
            run_service(
                &storage,
                source,
                LocalDownloader::new(storage.clone()),
                info,
                backend,
            )
            .await;
            storage.close().await;
        });
        let CoreEvent::Recommendations(initial) = event_until(
            &ui.events,
            |event| matches!(event, CoreEvent::Recommendations(state) if state.loaded),
        )
        .await?
        else {
            anyhow::bail!("inbox missing");
        };
        assert_eq!(
            initial.monitoring,
            attempt == 1,
            "saved monitoring intent must restore"
        );
        if attempt == 0 {
            assert_eq!(polls.load(Ordering::SeqCst), 0);
        }
        if attempt == 0 {
            ui.commands
                .send(CoreCommand::SetMonitoring {
                    account: 1,
                    enabled: true,
                })
                .await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::Recommendations(state) if state.checking.is_some())).await?;
            let CoreEvent::Recommendations(baseline) = event_until(&ui.events, |event| matches!(event, CoreEvent::Recommendations(state) if state.checking.is_none())).await?
            else { anyhow::bail!("baseline missing"); };
            assert!(
                baseline.items.is_empty(),
                "first enable must not recommend old history"
            );
            ui.commands
                .send(CoreCommand::SetMonitoring {
                    account: 1,
                    enabled: false,
                })
                .await?;
            event_until(&ui.events, |event| {
                matches!(event, CoreEvent::RecommendationActionFinished)
            })
            .await?;
            ui.commands
                .send(CoreCommand::SetMonitoring {
                    account: 1,
                    enabled: true,
                })
                .await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::Recommendations(state) if state.checking.is_some())).await?;
            let CoreEvent::Recommendations(found) = event_until(&ui.events, |event| matches!(event, CoreEvent::Recommendations(state) if state.checking.is_none())).await?
            else { anyhow::bail!("findings missing"); };
            assert_eq!(found.items.len(), 2);
            assert_eq!(found.unread_count(), 2);
            assert!(
                inspection.list_downloads().await?.is_empty(),
                "discovery must not create transfer history"
            );
            assert_eq!(std::fs::read_dir(paths.download_directory())?.count(), 0);
            ui.commands
                .send(CoreCommand::ChangeRecommendations {
                    account: 1,
                    keys: vec![book().key],
                    status: Status::Read,
                })
                .await?;
            event_until(&ui.events, |event| {
                matches!(event, CoreEvent::RecommendationActionFinished)
            })
            .await?;
            ui.commands
                .send(CoreCommand::ChangeRecommendations {
                    account: 1,
                    keys: vec![books()[2].key],
                    status: Status::Hidden,
                })
                .await?;
            event_until(&ui.events, |event| {
                matches!(event, CoreEvent::RecommendationActionFinished)
            })
            .await?;
            let items = inspection.load_recommendations(1).await?;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].status, Status::Read);
        } else {
            assert_eq!(initial.items.len(), 1);
            assert_eq!(initial.items[0].status, Status::Read);
            assert_eq!(initial.unread_count(), 0);
            ui.commands
                .send(CoreCommand::SetMonitoring {
                    account: 1,
                    enabled: false,
                })
                .await?;
            event_until(&ui.events, |event| {
                matches!(event, CoreEvent::RecommendationActionFinished)
            })
            .await?;
            assert!(!inspection.load_monitoring_enabled(1).await?);
            polls.store(0, Ordering::SeqCst);
            ui.commands
                .send(CoreCommand::DownloadRecommendations {
                    account: 1,
                    keys: vec![book().key],
                })
                .await?;
            event_until(
                &ui.events,
                |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_some()),
            )
            .await?;
            ui.commands
                .send(CoreCommand::SetMonitoring {
                    account: 1,
                    enabled: true,
                })
                .await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadUpdated(record)
                if record.progress.downloaded_bytes() == 4 && record.status == DownloadStatus::Downloading)).await?;
            assert_eq!(
                polls.load(Ordering::SeqCst),
                0,
                "monitoring must not share the connection with an active transfer"
            );
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.status == DownloadStatus::Completed)).await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::Recommendations(state) if state.items.is_empty() && state.checking.is_none())).await?;
            let history = inspection.list_downloads().await?;
            assert_eq!(history.len(), 1);
            assert_eq!(tokio::fs::read(&history[0].local_path).await?, b"epubdata");
        }
        ui.commands.send(CoreCommand::Shutdown).await?;
        task.await?;
    }
    let reopened = Storage::open(&paths.database()).await?;
    assert!(reopened.load_recommendations(1).await?.is_empty());
    assert_eq!(std::fs::read_dir(paths.download_directory())?.count(), 1);
    reopened.close().await;
    Ok(())
}

#[tokio::test]
async fn saved_sources_restore_without_scanning_or_starting_downloads() -> Result<()> {
    use tgbooks_core::SourceStore as _;
    let temporary = tempfile::tempdir()?;
    let paths = AppPaths::in_directory(temporary.path().join("application"))?;
    let source_id = book().key.channel_id;
    for attempt in 0..3 {
        let (storage, info) = initialize(paths.clone()).await?;
        let (ui, backend) = tgbooks_core::connect();
        let task = tokio::spawn(async move {
            run_service(
                &storage,
                FakeTelegram,
                LocalDownloader::new(storage.clone()),
                info,
                backend,
            )
            .await;
            storage.close().await;
        });
        let CoreEvent::Catalog(catalog) = event_until(&ui.events, |event|
            matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy())).await?
        else { anyhow::bail!("catalog missing"); };
        assert!(catalog.sources.loaded);
        assert_eq!(catalog.sources.contains(source_id), attempt == 1);
        assert!(catalog.selected.is_none() && catalog.books.is_empty() && !catalog.scan_started);
        if attempt < 2 {
            ui.commands
                .send(CoreCommand::SetChannelSource {
                    id: source_id,
                    enabled: attempt == 0,
                })
                .await?;
            event_until(&ui.events, |event| {
                matches!(event, CoreEvent::SourceSaved(Ok(())))
            })
            .await?;
            // Inspect the next authoritative state, without triggering any Telegram RPC.
            ui.commands.send(CoreCommand::ReloadSources).await?;
            let CoreEvent::Catalog(catalog) =
                event_until(&ui.events, |event| matches!(event, CoreEvent::Catalog(_))).await?
            else {
                anyhow::bail!("sources missing");
            };
            assert_eq!(catalog.sources.contains(source_id), attempt == 0);
            assert!(catalog.selected.is_none() && !catalog.scan_started);
        }
        ui.commands.send(CoreCommand::Shutdown).await?;
        task.await?;
    }
    let reopened = Storage::open(&paths.database()).await?;
    assert_eq!(reopened.load_sources(1).await, Ok(Vec::new()));
    assert!(reopened.list_downloads().await?.is_empty());
    assert_eq!(std::fs::read_dir(paths.download_directory())?.count(), 0);
    reopened.close().await;
    Ok(())
}

#[tokio::test]
async fn repeat_to_default_survives_restart_without_erasing_the_original_copy() -> Result<()> {
    use tgbooks_core::QueueStore as _;
    let temporary = tempfile::tempdir()?;
    let paths = AppPaths::in_directory(temporary.path().join("application"))?;
    let (storage, _) = initialize(paths.clone()).await?;
    let original_folder = paths.download_directory().to_owned();
    let new_folder = temporary.path().join("Новая папка");
    tokio::fs::create_dir(&new_folder).await?;
    let settings = storage
        .save_settings(&AppSettings::new(new_folder.clone()))
        .await
        .map_err(|_| anyhow::anyhow!("settings not saved"))?;
    let source = FakeTelegram.open_book(book().key).await?;
    let (updates, _receiver) = async_channel::unbounded();
    use tgbooks_core::DownloadEngine as _;
    let original = LocalDownloader::new(storage.clone())
        .save(source, original_folder, updates, Default::default())
        .await?;
    storage.close().await;

    for attempt in 0..2 {
        let (storage, info) = initialize(paths.clone()).await?;
        assert_eq!(info.settings, settings);
        let (ui, backend) = tgbooks_core::connect();
        let task = tokio::spawn(async move {
            run_service(
                &storage,
                FakeTelegram,
                LocalDownloader::new(storage.clone()),
                info,
                backend,
            )
            .await;
            storage.close().await;
        });
        event_until(&ui.events, |event| matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy())).await?;
        if attempt == 0 {
            ui.commands
                .send(CoreCommand::SelectChannel(Some(book().key.channel_id)))
                .await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started && !state.busy())).await?;
            ui.commands.send(CoreCommand::SetQueuePaused(true)).await?;
            event_until(
                &ui.events,
                |event| matches!(event, CoreEvent::DownloadState(state) if state.paused),
            )
            .await?;
            ui.commands
                .send(CoreCommand::DownloadToDefault(vec![book().key]))
                .await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadState(state) if state.skipped_duplicates == 1 && state.queued.is_empty())).await?;
            ui.commands
                .send(CoreCommand::RedownloadBook {
                    key: book().key,
                    directory: None,
                })
                .await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadState(state)
                if state.paused && state.queued.len() == 1 && state.queued[0].redownload && state.queued[0].directory == settings.download_directory)).await?;
        } else {
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadState(state)
                if state.restored && state.paused && state.queued.len() == 1 && state.queued[0].redownload)).await?;
            // Restored metadata is sufficient: no need to rescan the history.
            ui.commands.send(CoreCommand::SetQueuePaused(false)).await?;
            event_until(&ui.events, |event| {
                matches!(event, CoreEvent::DownloadUpdated(record)
                if record.key == book().key && record.status == DownloadStatus::Completed)
            })
            .await?;
            event_until(
                &ui.events,
                |event| matches!(event, CoreEvent::HistoryLoaded(records) if records.len() == 2),
            )
            .await?;
        }
        ui.commands.send(CoreCommand::Shutdown).await?;
        task.await?;
    }
    let reopened = Storage::open(&paths.database()).await?;
    let history = reopened.list_downloads().await?;
    assert_eq!(history.len(), 2);
    assert!(history.contains(&original));
    assert!(reopened.load_queue(1).await?.is_empty());
    for record in history {
        assert_eq!(record.status, DownloadStatus::Completed);
        assert_eq!(tokio::fs::read(&record.local_path).await?, b"epubdata");
    }
    assert_eq!(std::fs::read_dir(new_folder)?.count(), 1);
    reopened.close().await;
    Ok(())
}

#[tokio::test]
async fn discovered_book_streams_to_disk_reports_progress_and_reopens_in_sqlite() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let paths = AppPaths::in_directory(temporary.path().join("application"))?;
    let (storage, info) = initialize(paths.clone()).await?;
    let directory = paths.download_directory().to_owned();
    let database = paths.database();
    let (ui, backend) = tgbooks_core::connect();
    let task = tokio::spawn(async move {
        run_service(
            &storage,
            FakeTelegram,
            LocalDownloader::new(storage.clone()),
            info,
            backend,
        )
        .await;
        storage.close().await;
    });
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy()),
    )
    .await?;
    ui.commands
        .send(CoreCommand::SelectChannel(Some(book().key.channel_id)))
        .await?;
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_some()),
    )
    .await?;
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started),
    )
    .await?;
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: vec![book().key],
            directory,
        })
        .await?;
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_some()),
    )
    .await?;
    let partial = event_until(&ui.events, |event| {
        matches!(event, CoreEvent::DownloadUpdated(record)
        if record.progress.downloaded_bytes() > 0 && record.progress.downloaded_bytes() < 8)
    })
    .await?;
    assert!(
        matches!(partial, CoreEvent::DownloadUpdated(record) if record.progress.percentage() == Some(50))
    );
    event_until(&ui.events, |event| {
        matches!(event, CoreEvent::DownloadMetrics { key, metrics }
            if *key == book().key && metrics.bytes_per_second.is_some_and(|rate| rate > 0)
                && metrics.remaining_seconds.is_some())
    })
    .await?;
    let CoreEvent::DownloadUpdated(completed) = event_until(&ui.events, |event| {
        matches!(event,
        CoreEvent::DownloadUpdated(record) if record.status == DownloadStatus::Completed)
    })
    .await?
    else {
        anyhow::bail!("completed record missing")
    };
    assert_eq!(tokio::fs::read(&completed.local_path).await?, b"epubdata");
    let CoreEvent::DownloadState(finished) = event_until(&ui.events, |event| {
        matches!(event,
        CoreEvent::DownloadState(state) if state.active.is_none())
    })
    .await?
    else {
        anyhow::bail!("completion state missing")
    };
    assert_eq!(finished.error, None);
    let CoreEvent::HistoryLoaded(records) = event_until(&ui.events, |event| {
        matches!(event, CoreEvent::HistoryLoaded(_))
    })
    .await?
    else {
        anyhow::bail!("history missing")
    };
    assert_eq!(records, vec![completed.clone()]);
    ui.commands.send(CoreCommand::Shutdown).await?;
    task.await?;
    let reopened = Storage::open(&database).await?;
    let records: Vec<DownloadRecord> = reopened.list_downloads().await?;
    assert_eq!(records, vec![completed]);
    assert_eq!(records[0].progress.percentage(), Some(100));
    reopened.close().await;
    Ok(())
}

#[tokio::test]
async fn batch_downloads_continue_after_source_error_and_deduplicate_after_restart() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let paths = AppPaths::in_directory(temporary.path().join("application"))?;
    let directory = paths.download_directory().to_owned();
    let database = paths.database();
    let attachments = books();
    for attempt in 0..2 {
        let (storage, info) = initialize(paths.clone()).await?;
        let (ui, backend) = tgbooks_core::connect();
        let task = tokio::spawn(async move {
            run_service(
                &storage,
                FakeTelegram,
                LocalDownloader::new(storage.clone()),
                info,
                backend,
            )
            .await;
            storage.close().await;
        });
        event_until(&ui.events, |event| matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy())).await?;
        ui.commands
            .send(CoreCommand::SelectChannel(Some(book().key.channel_id)))
            .await?;
        event_until(
            &ui.events,
            |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_some()),
        )
        .await?;
        event_until(
            &ui.events,
            |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started),
        )
        .await?;
        let keys: Vec<_> = attachments
            .iter()
            .filter(|book| attempt == 0 || book.key != attachments[1].key)
            .map(|book| book.key)
            .collect();
        ui.commands
            .send(CoreCommand::DownloadBooks {
                keys,
                directory: directory.clone(),
            })
            .await?;
        if attempt == 0 {
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadState(state) if state.error == Some(tgbooks_core::DownloadError::Network))).await?;
            // A transient source error is retained, not silently skipped.
            ui.commands
                .send(CoreCommand::CancelDownload(attachments[1].key))
                .await?;
            ui.commands.send(CoreCommand::SetQueuePaused(false)).await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadUpdated(record) if record.key == attachments[2].key && record.status == DownloadStatus::Completed)).await?;
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_none() && state.queued.is_empty())).await?;
        } else {
            event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadState(state) if state.active.is_none() && state.queued.is_empty() && state.skipped_duplicates == 2)).await?;
        }
        ui.commands.send(CoreCommand::Shutdown).await?;
        // Drain completion/history events so bounded UI backpressure cannot delay shutdown.
        while ui.events.recv().await.is_ok() {}
        task.await?;
    }
    let reopened = Storage::open(&database).await?;
    let records = reopened.list_downloads().await?;
    assert_eq!(records.len(), 2);
    for record in records {
        assert_eq!(record.status, DownloadStatus::Completed);
        assert_eq!(tokio::fs::read(&record.local_path).await?, b"epubdata");
    }
    reopened.close().await;
    assert_eq!(
        std::fs::read_dir(&directory)?.count(),
        2,
        "unexpected partial or duplicate files"
    );
    Ok(())
}

#[tokio::test]
async fn interrupted_file_and_waiting_intent_restore_paused_then_resume_from_checkpoint()
-> Result<()> {
    use tgbooks_core::QueueStore;
    let temporary = tempfile::tempdir()?;
    let paths = AppPaths::in_directory(temporary.path().join("application"))?;
    let directory = paths.download_directory().to_owned();
    let attachments = books();
    let keys = vec![attachments[0].key, attachments[2].key];
    let (storage, info) = initialize(paths.clone()).await?;
    let (ui, backend) = tgbooks_core::connect();
    let task = tokio::spawn(async move {
        run_service(
            &storage,
            FakeTelegram,
            LocalDownloader::new(storage.clone()),
            info,
            backend,
        )
        .await;
        storage.close().await;
    });
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::Catalog(state) if state.authenticated && !state.busy()),
    )
    .await?;
    ui.commands
        .send(CoreCommand::SelectChannel(Some(book().key.channel_id)))
        .await?;
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::Catalog(state) if state.selected.is_some()),
    )
    .await?;
    event_until(
        &ui.events,
        |event| matches!(event, CoreEvent::Catalog(state) if state.scan_started),
    )
    .await?;
    ui.commands
        .send(CoreCommand::DownloadBooks {
            keys: keys.clone(),
            directory: directory.clone(),
        })
        .await?;
    event_until(&ui.events, |event| {
        matches!(event, CoreEvent::DownloadUpdated(record)
        if record.key == keys[0] && record.progress.downloaded_bytes() == 4)
    })
    .await?;
    ui.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task).await??;

    let (storage, info) = initialize(paths.clone()).await?;
    let queued = storage.load_queue(1).await?;
    assert_eq!(
        queued.iter().map(|job| job.book.key).collect::<Vec<_>>(),
        keys
    );
    assert!(queued[0].interrupted);
    assert!(!queued[1].interrupted);
    assert!(queued.iter().all(|job| job.directory == directory));
    let (ui, backend) = tgbooks_core::connect();
    let task = tokio::spawn(async move {
        run_service(
            &storage,
            FakeTelegram,
            LocalDownloader::new(storage.clone()),
            info,
            backend,
        )
        .await;
        storage.close().await;
    });
    event_until(&ui.events, |event| {
        matches!(event, CoreEvent::DownloadState(state)
        if state.restored && state.paused && state.active.is_none() && state.queued == queued)
    })
    .await?;
    assert_eq!(
        std::fs::read_dir(&directory)?.count(),
        1,
        "only the previous attempt's partial file should exist"
    );
    // Continuing does not require discovering the same history page again.
    ui.commands.send(CoreCommand::SetQueuePaused(false)).await?;
    event_until(&ui.events, |event| matches!(event, CoreEvent::DownloadUpdated(record)
        if record.key == keys[0] && record.progress.downloaded_bytes() == 4 && record.status == DownloadStatus::Downloading)).await?;
    event_until(&ui.events, |event| {
        matches!(event, CoreEvent::DownloadUpdated(record)
        if record.key == keys[1] && record.status == DownloadStatus::Completed)
    })
    .await?;
    event_until(&ui.events, |event| {
        matches!(event, CoreEvent::DownloadState(state)
        if state.active.is_none() && state.queued.is_empty())
    })
    .await?;
    ui.commands.send(CoreCommand::Shutdown).await?;
    while ui.events.recv().await.is_ok() {}
    task.await?;
    let reopened = Storage::open(&paths.database()).await?;
    assert!(reopened.load_queue(1).await?.is_empty());
    let records = reopened.list_downloads().await?;
    assert_eq!(records.len(), 2);
    for record in records {
        assert_eq!(record.status, DownloadStatus::Completed);
        assert_eq!(tokio::fs::read(record.local_path).await?, b"epubdata");
    }
    assert_eq!(std::fs::read_dir(directory)?.count(), 2);
    reopened.close().await;
    Ok(())
}
