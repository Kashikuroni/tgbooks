use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use async_channel::Sender;
use tempfile::NamedTempFile;
use tgbooks_core::{
    BookAttachment, BookFormat, DownloadEngine, DownloadError, DownloadProgress, DownloadRecord,
    DownloadRepository, DownloadStatus, DownloadUpdate, RemoteBook, TransferMeter,
};
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct LocalDownloader<R> {
    repository: R,
}

impl<R: DownloadRepository> LocalDownloader<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
}

impl<R: DownloadRepository> DownloadEngine for LocalDownloader<R> {
    async fn recover(&self) -> Result<(), DownloadError> {
        self.repository.recover_interrupted().await
    }

    async fn save(
        &self,
        mut remote: RemoteBook,
        directory: PathBuf,
        updates: Sender<DownloadUpdate>,
        cancellation: CancellationToken,
    ) -> Result<DownloadRecord, DownloadError> {
        let (directory, name) = destination(&directory, &remote.book).await?;
        let path = directory.join(&name);
        let mut record = DownloadRecord {
            key: remote.book.key,
            file_name: name,
            local_path: path.to_str().ok_or(DownloadError::Destination)?.to_owned(),
            status: DownloadStatus::Queued,
            progress: DownloadProgress::new(0, remote.book.size_bytes)
                .map_err(|_| DownloadError::InvalidFile)?,
        };
        if cancellation.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        if !self.repository.claim(&record).await? {
            return Err(DownloadError::AlreadyDownloaded);
        }
        let _ = updates.try_send(record.clone().into());
        let result = self
            .transfer(
                &mut remote,
                &mut record,
                directory,
                path,
                &updates,
                &cancellation,
            )
            .await;
        if let Err(mut error) = result {
            record.status = DownloadStatus::Failed;
            if self.repository.update(&record).await.is_err() {
                error = DownloadError::Storage;
            }
            let _ = updates.try_send(record.into());
            return Err(error);
        }
        Ok(record)
    }
}

impl<R: DownloadRepository> LocalDownloader<R> {
    async fn transfer(
        &self,
        remote: &mut RemoteBook,
        record: &mut DownloadRecord,
        directory: PathBuf,
        path: PathBuf,
        updates: &Sender<DownloadUpdate>,
        cancellation: &CancellationToken,
    ) -> Result<(), DownloadError> {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(_) => return Err(DownloadError::FileExists),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(DownloadError::Destination),
        }
        // The temporary file and final file are on the same filesystem. No
        // remote filename is ever used as a path component without sanitizing.
        let temporary = tokio::task::spawn_blocking(move || {
            tempfile::Builder::new()
                .prefix(".tgbooks-")
                .suffix(".part")
                .tempfile_in(directory)
        })
        .await
        .map_err(|_| DownloadError::Internal)?
        .map_err(|_| DownloadError::Destination)?;
        let file = temporary
            .as_file()
            .try_clone()
            .map_err(|_| DownloadError::Destination)?;
        let mut file = tokio::fs::File::from_std(file);
        record.status = DownloadStatus::Downloading;
        self.repository.update(record).await?;
        let _ = updates.try_send(record.clone().into());
        let mut last_update = Instant::now();
        let started = Instant::now();
        let mut meter = TransferMeter::default();
        let period = Duration::from_secs(1);
        let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let copied = async {
            loop {
                let chunk = {
                    // Keep the same future alive across timer ticks. Grammers'
                    // DownloadIter::next is not cancellation-safe between chunks.
                    let next = remote.chunks.next_chunk();
                    tokio::pin!(next);
                    loop {
                        tokio::select! {
                            result = &mut next => break result?,
                            _ = heartbeat.tick() => {
                                let metrics = meter.observe(record.progress, started.elapsed());
                                let _ = updates.try_send(DownloadUpdate::Metrics { key: record.key, metrics });
                            }
                        }
                    }
                };
                let Some(chunk) = chunk else { break };
                // A faulty adapter must not turn empty chunks into an endless loop.
                if chunk.is_empty() {
                    return Err(DownloadError::Incomplete);
                }
                let bytes = record
                    .progress
                    .downloaded_bytes()
                    .checked_add(chunk.len() as u64)
                    .ok_or(DownloadError::InvalidFile)?;
                let progress = DownloadProgress::new(bytes, record.progress.total_bytes())
                    .map_err(|_| DownloadError::Incomplete)?;
                file.write_all(&chunk)
                    .await
                    .map_err(|_| DownloadError::Destination)?;
                record.progress = progress;
                if last_update.elapsed() >= Duration::from_millis(250) {
                    self.repository.update(record).await?;
                    let _ = updates.try_send(record.clone().into());
                    let metrics = meter.observe(record.progress, started.elapsed());
                    let _ = updates.try_send(DownloadUpdate::Metrics {
                        key: record.key,
                        metrics,
                    });
                    last_update = Instant::now();
                }
            }
            if record
                .progress
                .total_bytes()
                .is_some_and(|total| total != record.progress.downloaded_bytes())
            {
                return Err(DownloadError::Incomplete);
            }
            file.flush().await.map_err(|_| DownloadError::Destination)?;
            file.sync_all()
                .await
                .map_err(|_| DownloadError::Destination)?;
            Ok(())
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(DownloadError::Cancelled),
            result = copied => result?,
        }
        drop(file);
        if cancellation.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        // Commit point: cancellation no longer interrupts publication + database
        // completion. A racing cancel may legitimately finish as Completed.
        publish(temporary, path).await?;
        record.status = DownloadStatus::Completed;
        let bytes = record.progress.downloaded_bytes();
        record.progress =
            DownloadProgress::new(bytes, Some(bytes)).map_err(|_| DownloadError::InvalidFile)?;
        self.repository.update(record).await?;
        let _ = updates.try_send(record.clone().into());
        Ok(())
    }
}

async fn destination(
    directory: &Path,
    book: &BookAttachment,
) -> Result<(PathBuf, String), DownloadError> {
    if !directory.is_absolute() {
        return Err(DownloadError::Destination);
    }
    if BookFormat::from_file_name(&book.file_name) != Some(book.format) {
        return Err(DownloadError::InvalidFile);
    }
    let directory = tokio::fs::canonicalize(directory)
        .await
        .map_err(|_| DownloadError::Destination)?;
    if !tokio::fs::metadata(&directory)
        .await
        .map_err(|_| DownloadError::Destination)?
        .is_dir()
    {
        return Err(DownloadError::Destination);
    }
    Ok((directory, safe_file_name(book)))
}

fn safe_file_name(book: &BookAttachment) -> String {
    // Treat both separators as separators on every OS. Ignore directory parts,
    // controls, bidi controls, and all Windows-reserved filename punctuation.
    let leaf = book.file_name.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = leaf.rsplit_once('.').map_or(leaf, |(stem, _)| stem);
    let mut safe = String::new();
    for ch in stem.chars() {
        let ch = if ch.is_control()
            || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            '_'
        } else {
            ch
        };
        // Reserve enough space for identities + extension, including UTF-16.
        if safe.len() + ch.len_utf8() > 120 {
            break;
        }
        safe.push(ch);
    }
    let stem = safe.trim_matches([' ', '.']);
    let stem = if stem.is_empty() { "book" } else { stem };
    // The prefix defeats Windows device names (CON/NUL/COM1, even with suffixes).
    // Stable Telegram identities prevent collisions between unrelated books.
    format!(
        "book-{stem}--{}-{}-{}.{}",
        book.key.channel_id,
        book.key.message_id,
        book.key.document_id,
        book.format.extension()
    )
}

async fn publish(temporary: NamedTempFile, path: PathBuf) -> Result<(), DownloadError> {
    tokio::task::spawn_blocking(move || {
        temporary
            .persist_noclobber(path)
            .map(drop)
            .map_err(|error| {
                if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                    DownloadError::FileExists
                } else {
                    DownloadError::Destination
                }
            })
    })
    .await
    .map_err(|_| DownloadError::Internal)?
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
