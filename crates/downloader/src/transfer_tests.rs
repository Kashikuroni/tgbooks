use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use tgbooks_core::{BookChunks, ChunkFuture, DownloadHistory, DownloadKey};
use tgbooks_storage::Storage;

use super::*;

struct Chunks {
    chunks: VecDeque<Result<Vec<u8>, DownloadError>>,
    calls: Arc<AtomicUsize>,
}

impl BookChunks for Chunks {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.chunks.pop_front().transpose()
        })
    }
}

fn book(name: &str, size: Option<u64>) -> BookAttachment {
    BookAttachment {
        key: DownloadKey {
            channel_id: -1000000000001,
            message_id: 42,
            document_id: 99,
        },
        file_name: name.into(),
        size_bytes: size,
        format: BookFormat::Epub,
    }
}

fn remote(
    book: BookAttachment,
    chunks: Vec<Result<Vec<u8>, DownloadError>>,
) -> (RemoteBook, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        RemoteBook {
            book,
            chunks: Box::new(Chunks {
                chunks: chunks.into(),
                calls: calls.clone(),
            }),
        },
        calls,
    )
}

async fn setup() -> Result<
    (
        tempfile::TempDir,
        PathBuf,
        Storage,
        LocalDownloader<Storage>,
    ),
    Box<dyn std::error::Error>,
> {
    let directory = tempfile::tempdir()?;
    let books = directory.path().join("Книги 世界");
    tokio::fs::create_dir(&books).await?;
    let storage = Storage::open(&directory.path().join("downloads.sqlite3")).await?;
    let downloader = LocalDownloader::new(storage.clone());
    Ok((directory, books, storage, downloader))
}

fn progress() -> (
    Sender<DownloadUpdate>,
    async_channel::Receiver<DownloadUpdate>,
) {
    async_channel::bounded(16)
}

#[tokio::test]
async fn streamed_bytes_history_and_deduplication_survive_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let (directory, books, storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    let attachment = book("Автор — Книга.EPUB", Some(10));
    let (source, calls) = remote(
        attachment.clone(),
        vec![Ok(b"hello".to_vec()), Ok(b"world".to_vec())],
    );
    let saved = downloader
        .save(
            source,
            books.clone(),
            updates.clone(),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(saved.status, DownloadStatus::Completed);
    assert_eq!(saved.progress.percentage(), Some(100));
    assert_eq!(tokio::fs::read(&saved.local_path).await?, b"helloworld");
    assert_eq!(
        Path::new(&saved.local_path).parent(),
        Some(tokio::fs::canonicalize(&books).await?.as_path())
    );
    assert_eq!(std::fs::read_dir(&books)?.count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    storage.close().await;
    let reopened = Storage::open(&directory.path().join("downloads.sqlite3")).await?;
    assert_eq!(reopened.list_downloads().await?, vec![saved]);
    let downloader = LocalDownloader::new(reopened);
    let (source, calls) = remote(attachment, vec![Ok(b"do not read".to_vec())]);
    assert_eq!(
        downloader
            .save(source, books, updates, CancellationToken::new())
            .await,
        Err(DownloadError::AlreadyDownloaded)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn unknown_and_empty_sizes_finish_with_actual_length()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, books, _storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    let (source, _) = remote(book("unknown.epub", None), vec![Ok(b"abc".to_vec())]);
    let saved = downloader
        .save(
            source,
            books.clone(),
            updates.clone(),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(saved.progress, DownloadProgress::new(3, Some(3))?);
    let mut empty = book("empty.epub", Some(0));
    empty.key.message_id += 1;
    let (source, _) = remote(empty, vec![]);
    let saved = downloader
        .save(source, books, updates, CancellationToken::new())
        .await?;
    assert_eq!(saved.progress, DownloadProgress::new(0, Some(0))?);
    assert!(tokio::fs::read(saved.local_path).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn failed_transfer_is_not_published_and_can_be_retried_from_zero()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, books, storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    let attachment = book("book.epub", Some(6));
    let (source, _) = remote(
        attachment.clone(),
        vec![Ok(b"abc".to_vec()), Err(DownloadError::Network)],
    );
    assert_eq!(
        downloader
            .save(
                source,
                books.clone(),
                updates.clone(),
                CancellationToken::new()
            )
            .await,
        Err(DownloadError::Network)
    );
    assert_eq!(std::fs::read_dir(&books)?.count(), 0);
    let records = storage.list_downloads().await?;
    assert_eq!(records[0].status, DownloadStatus::Failed);
    assert_eq!(records[0].progress.downloaded_bytes(), 3);
    let (source, _) = remote(attachment, vec![Ok(b"abcdef".to_vec())]);
    let saved = downloader
        .save(source, books.clone(), updates, CancellationToken::new())
        .await?;
    assert_eq!(tokio::fs::read(saved.local_path).await?, b"abcdef");
    assert_eq!(storage.list_downloads().await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn wrong_length_and_empty_chunks_never_publish_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, books, storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    for chunks in [
        vec![Ok(b"a".to_vec())],
        vec![Ok(b"too long".to_vec())],
        vec![Ok(Vec::new())],
    ] {
        let (source, _) = remote(book("book.epub", Some(3)), chunks);
        assert_eq!(
            downloader
                .save(
                    source,
                    books.clone(),
                    updates.clone(),
                    CancellationToken::new()
                )
                .await,
            Err(DownloadError::Incomplete)
        );
        assert_eq!(std::fs::read_dir(&books)?.count(), 0);
        assert_eq!(
            storage.list_downloads().await?[0].status,
            DownloadStatus::Failed
        );
    }
    Ok(())
}

#[tokio::test]
async fn existing_file_is_not_overwritten_or_downloaded_again()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, books, _storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    let attachment = book("book.epub", Some(3));
    let path = books.join(safe_file_name(&attachment));
    tokio::fs::write(&path, b"keep me").await?;
    let (source, calls) = remote(attachment, vec![Ok(b"abc".to_vec())]);
    assert_eq!(
        downloader
            .save(source, books, updates, CancellationToken::new())
            .await,
        Err(DownloadError::FileExists)
    );
    assert_eq!(tokio::fs::read(path).await?, b"keep me");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn file_created_during_transfer_wins_without_clobbering()
-> Result<(), Box<dyn std::error::Error>> {
    struct RacingChunks {
        path: PathBuf,
        done: bool,
    }
    impl BookChunks for RacingChunks {
        fn next_chunk(&mut self) -> ChunkFuture<'_> {
            Box::pin(async {
                if self.done {
                    return Ok(None);
                }
                self.done = true;
                tokio::fs::write(&self.path, b"another application")
                    .await
                    .map_err(|_| DownloadError::Destination)?;
                Ok(Some(b"abc".to_vec()))
            })
        }
    }
    let (_directory, books, _storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    let attachment = book("book.epub", Some(3));
    let path = books.join(safe_file_name(&attachment));
    let source = RemoteBook {
        book: attachment,
        chunks: Box::new(RacingChunks {
            path: path.clone(),
            done: false,
        }),
    };
    assert_eq!(
        downloader
            .save(source, books.clone(), updates, CancellationToken::new())
            .await,
        Err(DownloadError::FileExists)
    );
    assert_eq!(tokio::fs::read(path).await?, b"another application");
    assert_eq!(std::fs::read_dir(books)?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn telemetry_ticks_while_waiting_without_restarting_the_in_flight_chunk()
-> Result<(), Box<dyn std::error::Error>> {
    struct GatedChunks {
        calls: Arc<AtomicUsize>,
        release: async_channel::Receiver<()>,
        sent: bool,
    }
    impl BookChunks for GatedChunks {
        fn next_chunk(&mut self) -> ChunkFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                if self.sent {
                    return Ok(None);
                }
                self.release
                    .recv()
                    .await
                    .map_err(|_| DownloadError::Network)?;
                self.sent = true;
                Ok(Some(b"data".to_vec()))
            })
        }
    }
    let (_directory, books, storage, downloader) = setup().await?;
    let (updates, receiver) = progress();
    let (release, gate) = async_channel::bounded(1);
    let calls = Arc::new(AtomicUsize::new(0));
    let source = RemoteBook {
        book: book("book.epub", Some(4)),
        chunks: Box::new(GatedChunks {
            calls: calls.clone(),
            release: gate,
            sent: false,
        }),
    };
    let job = tokio::spawn(async move {
        downloader
            .save(source, books, updates, CancellationToken::new())
            .await
    });
    let metrics = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let DownloadUpdate::Metrics { metrics, .. } = receiver.recv().await? {
                break Ok::<_, async_channel::RecvError>(metrics);
            }
        }
    })
    .await??;
    assert_eq!(metrics.bytes_per_second, Some(0));
    assert_eq!(metrics.remaining_seconds, None);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "heartbeat dropped and recreated the chunk future"
    );
    let records = storage.list_downloads().await?;
    assert_eq!(records[0].status, DownloadStatus::Downloading);
    assert_eq!(records[0].progress.downloaded_bytes(), 0);
    release.send(()).await?;
    let saved = tokio::time::timeout(Duration::from_secs(2), job).await???;
    assert_eq!(tokio::fs::read(&saved.local_path).await?, b"data");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_pending_chunks_and_cleans_the_temporary_file()
-> Result<(), Box<dyn std::error::Error>> {
    struct PendingChunks(Arc<tokio::sync::Notify>);
    impl BookChunks for PendingChunks {
        fn next_chunk(&mut self) -> ChunkFuture<'_> {
            Box::pin(async {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    let (_directory, books, storage, downloader) = setup().await?;
    let (updates, _receiver) = progress();
    let started = Arc::new(tokio::sync::Notify::new());
    let source = RemoteBook {
        book: book("book.epub", Some(3)),
        chunks: Box::new(PendingChunks(started.clone())),
    };
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let folder = books.clone();
    let job =
        tokio::spawn(async move { downloader.save(source, folder, updates, cancellation).await });
    tokio::time::timeout(Duration::from_secs(1), started.notified()).await?;
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), job).await??,
        Err(DownloadError::Cancelled)
    );
    assert_eq!(std::fs::read_dir(books)?.count(), 0);
    assert_eq!(
        storage.list_downloads().await?[0].status,
        DownloadStatus::Failed
    );
    Ok(())
}

#[tokio::test]
async fn aborted_worker_leaves_no_file_and_startup_recovery_marks_it_failed()
-> Result<(), Box<dyn std::error::Error>> {
    struct Blocked(Arc<tokio::sync::Notify>);
    impl BookChunks for Blocked {
        fn next_chunk(&mut self) -> ChunkFuture<'_> {
            Box::pin(async {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    let (_directory, books, storage, downloader) = setup().await?;
    let recover = downloader.clone();
    let (updates, _receiver) = progress();
    let started = Arc::new(tokio::sync::Notify::new());
    let source = RemoteBook {
        book: book("book.epub", Some(3)),
        chunks: Box::new(Blocked(started.clone())),
    };
    let folder = books.clone();
    let job = tokio::spawn(async move {
        downloader
            .save(source, folder, updates, CancellationToken::new())
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified()).await?;
    job.abort();
    assert!(job.await.is_err());
    assert_eq!(std::fs::read_dir(books)?.count(), 0);
    assert_eq!(
        storage.list_downloads().await?[0].status,
        DownloadStatus::Downloading
    );
    recover.recover().await?;
    assert_eq!(
        storage.list_downloads().await?[0].status,
        DownloadStatus::Failed
    );
    Ok(())
}

#[tokio::test]
async fn failure_after_publication_preserves_the_file_and_does_not_claim_success()
-> Result<(), Box<dyn std::error::Error>> {
    #[derive(Clone)]
    struct FailCompletion(Storage);
    impl DownloadRepository for FailCompletion {
        async fn claim(&self, record: &DownloadRecord) -> Result<bool, DownloadError> {
            self.0.claim(record).await
        }
        async fn update(&self, record: &DownloadRecord) -> Result<(), DownloadError> {
            if record.status == DownloadStatus::Completed {
                Err(DownloadError::Storage)
            } else {
                self.0.update(record).await
            }
        }
        async fn recover_interrupted(&self) -> Result<(), DownloadError> {
            self.0.recover_interrupted().await
        }
    }
    let (_directory, books, storage, _) = setup().await?;
    let downloader = LocalDownloader::new(FailCompletion(storage.clone()));
    let attachment = book("book.epub", Some(3));
    let path = books.join(safe_file_name(&attachment));
    let (source, _) = remote(attachment, vec![Ok(b"abc".to_vec())]);
    let (updates, _receiver) = progress();
    assert_eq!(
        downloader
            .save(source, books, updates, CancellationToken::new())
            .await,
        Err(DownloadError::Storage)
    );
    assert_eq!(tokio::fs::read(path).await?, b"abc");
    assert_eq!(
        storage.list_downloads().await?[0].status,
        DownloadStatus::Failed
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_destination_symlinks_are_never_followed() -> Result<(), Box<dyn std::error::Error>>
{
    let (directory, books, _storage, downloader) = setup().await?;
    let attachment = book("book.epub", Some(3));
    let outside = directory.path().join("outside.epub");
    std::os::unix::fs::symlink(&outside, books.join(safe_file_name(&attachment)))?;
    let (source, calls) = remote(attachment, vec![Ok(b"abc".to_vec())]);
    let (updates, _receiver) = progress();
    assert_eq!(
        downloader
            .save(source, books, updates, CancellationToken::new())
            .await,
        Err(DownloadError::FileExists)
    );
    assert!(!outside.exists());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn failed_database_admission_does_not_create_files_or_read_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, books, storage, downloader) = setup().await?;
    storage.close().await;
    let (updates, _receiver) = progress();
    let (source, calls) = remote(book("book.epub", Some(3)), vec![Ok(b"abc".to_vec())]);
    assert_eq!(
        downloader
            .save(source, books.clone(), updates, CancellationToken::new())
            .await,
        Err(DownloadError::Storage)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(books)?.count(), 0);
    Ok(())
}

#[test]
fn untrusted_filenames_are_single_portable_bounded_components() {
    for name in [
        "../../escape.epub".into(),
        r"C:\books\..\NUL.EPUB".into(),
        "CON.epub".into(),
        "../a:<b>?*|\n\u{202e}.epub".into(),
        format!("{}.epub", "世界🙂".repeat(200)),
    ] {
        let attachment = book(&name, None);
        let safe = safe_file_name(&attachment);
        assert_eq!(Path::new(&safe).components().count(), 1);
        assert!(
            !safe
                .chars()
                .any(|ch| ch.is_control() || r#"\/<>:"|?*"#.contains(ch))
        );
        assert!(safe.len() < 255);
        assert!(safe.starts_with("book-"));
        assert!(safe.ends_with(".epub"));
    }
    let first = book("same.epub", None);
    let mut second = first.clone();
    second.key.channel_id -= 1;
    assert_ne!(safe_file_name(&first), safe_file_name(&second));
}
