use super::*;
use std::sync::atomic::AtomicU64;
use tgbooks_core::DownloadCheckpoint;

/// This source exposes its seek offset, so matching final bytes cannot conceal
/// a full re-download or an off-by-one error.
struct SeekableBytes {
    bytes: Vec<u8>,
    offset: usize,
    observed: Arc<AtomicU64>,
}

impl BookChunks for SeekableBytes {
    fn seek(&mut self, offset: u64) -> Result<(), DownloadError> {
        self.offset = usize::try_from(offset).map_err(|_| DownloadError::InvalidFile)?;
        self.observed.store(offset, Ordering::SeqCst);
        Ok(())
    }
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async {
            if self.offset >= self.bytes.len() {
                return Ok(None);
            }
            let chunk = self.bytes[self.offset..].to_vec();
            self.offset = self.bytes.len();
            Ok(Some(chunk))
        })
    }
}

fn seekable(attachment: BookAttachment, bytes: &[u8]) -> (RemoteBook, Arc<AtomicU64>) {
    let observed = Arc::new(AtomicU64::new(u64::MAX));
    (
        RemoteBook {
            book: attachment,
            source_title: None,
            chunks: Box::new(SeekableBytes {
                bytes: bytes.to_vec(),
                offset: 0,
                observed: observed.clone(),
            }),
        },
        observed,
    )
}

async fn interrupted(
    storage: &Storage,
    downloader: &LocalDownloader<Storage>,
    books: &Path,
) -> Result<(DownloadRecord, DownloadCheckpoint), Box<dyn std::error::Error>> {
    let (source, _) = remote(
        book("book.epub", Some(6)),
        vec![Ok(b"abc".to_vec()), Err(DownloadError::Network)],
    );
    let (updates, _) = async_channel::unbounded();
    assert_eq!(
        downloader
            .save(source, books.to_owned(), updates, CancellationToken::new())
            .await,
        Err(DownloadError::Network)
    );
    let record = storage.list_downloads().await?.remove(0);
    let checkpoint = storage
        .checkpoint(&record)
        .await?
        .ok_or("missing checkpoint")?;
    assert_eq!(checkpoint.downloaded_bytes, 3);
    assert_eq!(tokio::fs::read(&checkpoint.part_path).await?, b"abc");
    Ok((record, checkpoint))
}

#[tokio::test]
async fn reopening_resumes_exactly_after_the_synced_prefix_and_clears_the_checkpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let (root, books, storage, downloader) = setup().await?;
    let (record, checkpoint) = interrupted(&storage, &downloader, &books).await?;
    storage.close().await;
    let reopened = Storage::open(&root.path().join("downloads.sqlite3")).await?;
    assert_eq!(reopened.checkpoint(&record).await?, Some(checkpoint));
    let (source, offset) = seekable(book("book.epub", Some(6)), b"abcdef");
    let (updates, receiver) = async_channel::unbounded();
    let saved = LocalDownloader::new(reopened.clone())
        .save(source, books.clone(), updates, CancellationToken::new())
        .await?;
    assert_eq!(offset.load(Ordering::SeqCst), 3);
    assert_eq!(tokio::fs::read(&saved.local_path).await?, b"abcdef");
    assert!(std::iter::from_fn(|| receiver.try_recv().ok()).any(|update| matches!(update, DownloadUpdate::Record(record)
        if record.status == DownloadStatus::Downloading && record.progress.downloaded_bytes() == 3)));
    assert!(reopened.checkpoint(&saved).await?.is_none());
    assert_eq!(std::fs::read_dir(&books)?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn completed_checkpoints_reconcile_publication_for_known_and_unknown_sizes()
-> Result<(), Box<dyn std::error::Error>> {
    for total in [Some(6), None] {
        for already_published in [false, true] {
            let (_root, books, storage, downloader) = setup().await?;
            let (record, mut checkpoint) = interrupted(&storage, &downloader, &books).await?;
            tokio::fs::write(&checkpoint.part_path, b"abcdef").await?;
            checkpoint.downloaded_bytes = 6;
            checkpoint.total_bytes = total;
            checkpoint.sha256 = sha2::Sha256::digest(b"abcdef").into();
            checkpoint.finished = true;
            storage.save_checkpoint(&record, &checkpoint).await?;
            if already_published {
                tokio::fs::rename(&checkpoint.part_path, &record.local_path).await?;
            }
            let (source, offset) = seekable(book("book.epub", total), b"WRONG!");
            let (updates, _) = async_channel::unbounded();
            let saved = downloader
                .save(source, books.clone(), updates, CancellationToken::new())
                .await?;
            assert_eq!(saved.status, DownloadStatus::Completed);
            assert_eq!(
                offset.load(Ordering::SeqCst),
                if already_published { u64::MAX } else { 6 }
            );
            assert_eq!(tokio::fs::read(&saved.local_path).await?, b"abcdef");
            assert_eq!(std::fs::read_dir(&books)?.count(), 1);
            assert!(storage.checkpoint(&record).await?.is_none());
        }
    }
    Ok(())
}

#[tokio::test]
async fn an_explicit_second_copy_resumes_without_modifying_the_original()
-> Result<(), Box<dyn std::error::Error>> {
    let (root, books, storage, downloader) = setup().await?;
    let (updates, _) = async_channel::unbounded();
    let attachment = book("book.epub", Some(6));
    let (source, _) = seekable(attachment.clone(), b"abcdef");
    let original = downloader
        .save(
            source,
            books.clone(),
            updates.clone(),
            CancellationToken::new(),
        )
        .await?;
    let other = root.path().join("second-copy");
    tokio::fs::create_dir(&other).await?;
    let (source, _) = remote(
        attachment.clone(),
        vec![Ok(b"abc".to_vec()), Err(DownloadError::Network)],
    );
    assert_eq!(
        downloader
            .save_again(
                source,
                other.clone(),
                updates.clone(),
                CancellationToken::new()
            )
            .await,
        Err(DownloadError::Network)
    );
    let (source, offset) = seekable(attachment, b"abcdef");
    let copy = downloader
        .save_again(source, other, updates, CancellationToken::new())
        .await?;
    assert_eq!(offset.load(Ordering::SeqCst), 3);
    assert_eq!(tokio::fs::read(&original.local_path).await?, b"abcdef");
    assert_eq!(tokio::fs::read(&copy.local_path).await?, b"abcdef");
    assert_eq!(storage.list_downloads().await?.len(), 2);
    assert!(storage.checkpoint(&copy).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn uncommitted_tail_is_truncated_before_appending() -> Result<(), Box<dyn std::error::Error>>
{
    let (_root, books, storage, downloader) = setup().await?;
    let (_, checkpoint) = interrupted(&storage, &downloader, &books).await?;
    tokio::fs::write(&checkpoint.part_path, b"abcUNCOMMITTED").await?;
    let (source, offset) = seekable(book("book.epub", Some(6)), b"abcdef");
    let (updates, _) = async_channel::unbounded();
    let saved = downloader
        .save(source, books, updates, CancellationToken::new())
        .await?;
    assert_eq!(offset.load(Ordering::SeqCst), 3);
    assert_eq!(tokio::fs::read(saved.local_path).await?, b"abcdef");
    Ok(())
}

#[tokio::test]
async fn truncated_corrupt_and_missing_parts_restart_safely_from_zero()
-> Result<(), Box<dyn std::error::Error>> {
    for data in [Some(b"a".as_slice()), Some(b"BAD".as_slice()), None] {
        let (_root, books, storage, downloader) = setup().await?;
        let (_, checkpoint) = interrupted(&storage, &downloader, &books).await?;
        if let Some(data) = data {
            tokio::fs::write(&checkpoint.part_path, data).await?;
        } else {
            tokio::fs::remove_file(&checkpoint.part_path).await?;
        }
        let (source, offset) = seekable(book("book.epub", Some(6)), b"abcdef");
        let (updates, _) = async_channel::unbounded();
        let saved = downloader
            .save(source, books.clone(), updates, CancellationToken::new())
            .await?;
        assert_eq!(offset.load(Ordering::SeqCst), 0);
        assert_eq!(tokio::fs::read(saved.local_path).await?, b"abcdef");
        assert_eq!(std::fs::read_dir(&books)?.count(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn changed_total_never_reuses_a_prefix_of_different_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let (_root, books, storage, downloader) = setup().await?;
    interrupted(&storage, &downloader, &books).await?;
    let (source, offset) = seekable(book("book.epub", Some(9)), b"new-bytes");
    let (updates, _) = async_channel::unbounded();
    let saved = downloader
        .save(source, books.clone(), updates, CancellationToken::new())
        .await?;
    assert_eq!(offset.load(Ordering::SeqCst), 0);
    assert_eq!(tokio::fs::read(saved.local_path).await?, b"new-bytes");
    assert_eq!(std::fs::read_dir(&books)?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn cancelling_a_waiting_interrupted_job_discards_only_its_partial_data()
-> Result<(), Box<dyn std::error::Error>> {
    let (_root, books, storage, downloader) = setup().await?;
    let (record, _) = interrupted(&storage, &downloader, &books).await?;
    let sentinel = books.join("other.epub");
    tokio::fs::write(&sentinel, b"keep").await?;
    downloader
        .discard(&QueuedDownload {
            book: book("book.epub", Some(6)),
            directory: books.clone(),
            interrupted: true,
            redownload: false,
        })
        .await?;
    assert!(storage.checkpoint(&record).await?.is_none());
    assert_eq!(tokio::fs::read(sentinel).await?, b"keep");
    assert_eq!(std::fs::read_dir(&books)?.count(), 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_part_is_not_followed_and_discard_never_deletes_its_target()
-> Result<(), Box<dyn std::error::Error>> {
    let (root, books, storage, downloader) = setup().await?;
    let (_, checkpoint) = interrupted(&storage, &downloader, &books).await?;
    let outside = root.path().join("outside");
    tokio::fs::write(&outside, b"private").await?;
    tokio::fs::remove_file(&checkpoint.part_path).await?;
    std::os::unix::fs::symlink(&outside, &checkpoint.part_path)?;
    let (source, offset) = seekable(book("book.epub", Some(6)), b"abcdef");
    let (updates, _) = async_channel::unbounded();
    assert_eq!(
        downloader
            .save(source, books.clone(), updates, CancellationToken::new())
            .await,
        Err(DownloadError::Destination)
    );
    assert_eq!(offset.load(Ordering::SeqCst), u64::MAX);
    downloader
        .discard(&QueuedDownload {
            book: book("book.epub", Some(6)),
            directory: books,
            interrupted: true,
            redownload: false,
        })
        .await?;
    assert_eq!(tokio::fs::read(outside).await?, b"private");
    Ok(())
}

#[tokio::test]
async fn checkpoint_with_an_out_of_directory_path_cannot_read_or_delete_that_file()
-> Result<(), Box<dyn std::error::Error>> {
    let (root, books, storage, downloader) = setup().await?;
    let (record, mut checkpoint) = interrupted(&storage, &downloader, &books).await?;
    let outside = root.path().join(".tgbooks-outside.part");
    tokio::fs::write(&outside, b"private").await?;
    checkpoint.part_path = outside.clone();
    storage.save_checkpoint(&record, &checkpoint).await?;
    let (source, _) = seekable(book("book.epub", Some(6)), b"abcdef");
    let (updates, _) = async_channel::unbounded();
    assert_eq!(
        downloader
            .save(source, books, updates, CancellationToken::new())
            .await,
        Err(DownloadError::Destination)
    );
    assert_eq!(tokio::fs::read(outside).await?, b"private");
    Ok(())
}
