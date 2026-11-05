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
                chunks: Box::new(OfflineChunks(0)),
            })
        })
    }
}

struct OfflineChunks(usize);
impl BookChunks for OfflineChunks {
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
    ui.commands.send(CoreCommand::ScanNextPage).await?;
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
        ui.commands.send(CoreCommand::ScanNextPage).await?;
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
