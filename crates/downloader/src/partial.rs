//! Filesystem half of a checkpoint. SQLite is authoritative only after sync_data.
use std::{io::SeekFrom, path::Path};

use sha2::{Digest, Sha256};
use tgbooks_core::{DownloadCheckpoint, DownloadError, DownloadRecord, DownloadRepository};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

pub(super) struct Partial {
    pub file: tokio::fs::File,
    pub checkpoint: DownloadCheckpoint,
    pub hasher: Sha256,
}

impl Partial {
    pub async fn open(
        repository: &impl DownloadRepository,
        record: &DownloadRecord,
        cancellation: &CancellationToken,
    ) -> Result<Self, DownloadError> {
        if let Some(checkpoint) = repository.checkpoint(record).await? {
            check_path(record, &checkpoint)?;
            let compatible = checkpoint.file_name == record.file_name
                && checkpoint.total_bytes == record.progress.total_bytes();
            if compatible {
                match tokio::fs::symlink_metadata(&checkpoint.part_path).await {
                    Ok(metadata)
                        if metadata.file_type().is_file()
                            && metadata.len() >= checkpoint.downloaded_bytes =>
                    {
                        let mut file = tokio::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(&checkpoint.part_path)
                            .await
                            .map_err(|_| DownloadError::Destination)?;
                        let hasher =
                            hash_prefix(&mut file, checkpoint.downloaded_bytes, cancellation)
                                .await?;
                        if hasher.clone().finalize().as_slice() == checkpoint.sha256 {
                            // An interrupted write/checkpoint may leave an uncommitted tail.
                            file.set_len(checkpoint.downloaded_bytes)
                                .await
                                .map_err(|_| DownloadError::Destination)?;
                            file.seek(SeekFrom::Start(checkpoint.downloaded_bytes))
                                .await
                                .map_err(|_| DownloadError::Destination)?;
                            return Ok(Self {
                                file,
                                checkpoint,
                                hasher,
                            });
                        }
                    }
                    Ok(metadata) if !metadata.file_type().is_file() => {
                        return Err(DownloadError::Destination);
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(DownloadError::Destination),
                }
            }
            discard(repository, record).await?;
        }
        let directory = Path::new(&record.local_path)
            .parent()
            .ok_or(DownloadError::Destination)?
            .to_owned();
        let temporary = tokio::task::spawn_blocking(move || {
            tempfile::Builder::new()
                .prefix(".tgbooks-")
                .suffix(".part")
                .tempfile_in(directory)
        })
        .await
        .map_err(|_| DownloadError::Internal)?
        .map_err(|_| DownloadError::Destination)?;
        let hasher = Sha256::new();
        let checkpoint = DownloadCheckpoint {
            part_path: temporary.path().to_owned(),
            file_name: record.file_name.clone(),
            total_bytes: record.progress.total_bytes(),
            downloaded_bytes: 0,
            sha256: hasher.clone().finalize().into(),
            finished: false,
        };
        // Until SQLite accepts this path, RAII removes it on error or abort.
        repository.save_checkpoint(record, &checkpoint).await?;
        let (file, _) = temporary.keep().map_err(|_| DownloadError::Destination)?;
        Ok(Self {
            file: tokio::fs::File::from_std(file),
            checkpoint,
            hasher,
        })
    }

    pub async fn commit(
        &mut self,
        repository: &impl DownloadRepository,
        record: &DownloadRecord,
    ) -> Result<(), DownloadError> {
        self.file
            .flush()
            .await
            .map_err(|_| DownloadError::Destination)?;
        self.file
            .sync_data()
            .await
            .map_err(|_| DownloadError::Destination)?;
        self.checkpoint.downloaded_bytes = record.progress.downloaded_bytes();
        self.checkpoint.sha256 = self.hasher.clone().finalize().into();
        repository.save_checkpoint(record, &self.checkpoint).await
    }
}

fn check_path(
    record: &DownloadRecord,
    checkpoint: &DownloadCheckpoint,
) -> Result<(), DownloadError> {
    let path = &checkpoint.part_path;
    let valid_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".tgbooks-") && name.ends_with(".part"));
    if !path.is_absolute() || path.parent() != Path::new(&record.local_path).parent() || !valid_name
    {
        return Err(DownloadError::Destination);
    }
    Ok(())
}

pub(super) async fn discard(
    repository: &impl DownloadRepository,
    record: &DownloadRecord,
) -> Result<(), DownloadError> {
    if let Some(checkpoint) = repository.checkpoint(record).await? {
        check_path(record, &checkpoint)?;
        // remove_file unlinks a symlink itself, never its target.
        match tokio::fs::remove_file(&checkpoint.part_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(DownloadError::Destination),
        }
        repository.clear_checkpoint(record).await?;
    }
    Ok(())
}

pub(super) async fn hash_prefix(
    file: &mut tokio::fs::File,
    bytes: u64,
    cancellation: &CancellationToken,
) -> Result<Sha256, DownloadError> {
    let mut hasher = Sha256::new();
    let mut remaining = bytes;
    let mut buffer = vec![0; 128 * 1024];
    while remaining > 0 {
        if cancellation.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let count = buffer.len().min(remaining.min(usize::MAX as u64) as usize);
        file.read_exact(&mut buffer[..count])
            .await
            .map_err(|_| DownloadError::Destination)?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(hasher)
}

/// Reconcile the crash window between no-clobber publication and SQLite completion.
pub(super) async fn published(
    repository: &impl DownloadRepository,
    record: &DownloadRecord,
    cancellation: &CancellationToken,
) -> Result<Option<u64>, DownloadError> {
    let path = Path::new(&record.local_path);
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(DownloadError::Destination),
    };
    let checkpoint = repository
        .checkpoint(record)
        .await?
        .ok_or(DownloadError::FileExists)?;
    check_path(record, &checkpoint)?;
    if !metadata.file_type().is_file()
        || !checkpoint.finished
        || checkpoint.downloaded_bytes != metadata.len()
        || checkpoint.file_name != record.file_name
        || checkpoint.total_bytes != record.progress.total_bytes()
    {
        return Err(DownloadError::FileExists);
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| DownloadError::Destination)?;
    let hash = hash_prefix(&mut file, metadata.len(), cancellation).await?;
    if hash.finalize().as_slice() != checkpoint.sha256 {
        return Err(DownloadError::FileExists);
    }
    // Part may have survived the link/unlink crash window.
    match tokio::fs::remove_file(&checkpoint.part_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(DownloadError::Destination),
    }
    Ok(Some(metadata.len()))
}
