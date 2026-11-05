//! SQLite and operating-system adapters. No UI or Telegram dependency.

mod lock;
mod paths;

use std::{path::Path, time::Duration};

pub use lock::{AppLock, AppLockError};
pub use paths::{AppPaths, PathsError};
use sqlx::{
    ConnectOptions, Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow},
};
use tgbooks_core::{
    DownloadError, DownloadHistory, DownloadKey, DownloadProgress, DownloadRecord,
    DownloadRepository, DownloadStatus,
};
use thiserror::Error;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("database migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("invalid or unsupported download data")]
    InvalidData,
}

#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    pub async fn open(path: &Path) -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            // Query values can contain private filenames; never log SQL statements.
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    /// The composite primary key makes deduplication atomic, not check-then-insert.
    pub async fn insert_download(&self, record: &DownloadRecord) -> Result<bool, StorageError> {
        let downloaded = i64::try_from(record.progress.downloaded_bytes())
            .map_err(|_| StorageError::InvalidData)?;
        let total = record
            .progress
            .total_bytes()
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StorageError::InvalidData)?;
        let result = sqlx::query(
            "INSERT INTO downloads
             (channel_id, message_id, document_id, file_name, local_path, status, downloaded_bytes, total_bytes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (channel_id, message_id, document_id) DO NOTHING",
        )
        .bind(record.key.channel_id)
        .bind(record.key.message_id)
        .bind(record.key.document_id)
        .bind(&record.file_name)
        .bind(&record.local_path)
        .bind(record.status.as_str())
        .bind(downloaded)
        .bind(total)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn contains(&self, key: DownloadKey) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM downloads
             WHERE channel_id = ? AND message_id = ? AND document_id = ?)",
        )
        .bind(key.channel_id)
        .bind(key.message_id)
        .bind(key.document_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

impl DownloadHistory for Storage {
    type Error = StorageError;

    async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
        let rows = sqlx::query(
            "SELECT channel_id, message_id, document_id, file_name, local_path, status,
                    downloaded_bytes, total_bytes
             FROM downloads ORDER BY created_at DESC, channel_id, message_id, document_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_record).collect()
    }
}

impl DownloadRepository for Storage {
    async fn claim(&self, record: &DownloadRecord) -> Result<bool, DownloadError> {
        if record.status != DownloadStatus::Queued || record.progress.downloaded_bytes() != 0 {
            return Err(DownloadError::Storage);
        }
        let (_, total) = sql_progress(record)?;
        let result = sqlx::query(
            "INSERT INTO downloads
             (channel_id, message_id, document_id, file_name, local_path, status, downloaded_bytes, total_bytes)
             VALUES (?, ?, ?, ?, ?, 'queued', 0, ?)
             ON CONFLICT (channel_id, message_id, document_id) DO UPDATE SET
               file_name = excluded.file_name, local_path = excluded.local_path,
               status = 'queued', downloaded_bytes = 0, total_bytes = excluded.total_bytes,
               created_at = unixepoch()
             WHERE downloads.status = 'failed'",
        )
        .bind(record.key.channel_id)
        .bind(record.key.message_id)
        .bind(record.key.document_id)
        .bind(&record.file_name)
        .bind(&record.local_path)
        .bind(total)
        .execute(&self.pool)
        .await.map_err(|_| DownloadError::Storage)?;
        Ok(result.rows_affected() == 1)
    }

    async fn update(&self, record: &DownloadRecord) -> Result<(), DownloadError> {
        let (downloaded, total) = sql_progress(record)?;
        let result = sqlx::query(
            "UPDATE downloads SET status = ?, downloaded_bytes = ?, total_bytes = ?
             WHERE channel_id = ? AND message_id = ? AND document_id = ?
               AND status IN ('queued', 'downloading') AND downloaded_bytes <= ?",
        )
        .bind(record.status.as_str())
        .bind(downloaded)
        .bind(total)
        .bind(record.key.channel_id)
        .bind(record.key.message_id)
        .bind(record.key.document_id)
        .bind(downloaded)
        .execute(&self.pool)
        .await
        .map_err(|_| DownloadError::Storage)?;
        if result.rows_affected() != 1 {
            return Err(DownloadError::Storage);
        }
        Ok(())
    }

    async fn recover_interrupted(&self) -> Result<(), DownloadError> {
        sqlx::query(
            "UPDATE downloads SET status = 'failed' WHERE status IN ('queued', 'downloading')",
        )
        .execute(&self.pool)
        .await
        .map_err(|_| DownloadError::Storage)?;
        Ok(())
    }
}

fn sql_progress(record: &DownloadRecord) -> Result<(i64, Option<i64>), DownloadError> {
    let downloaded =
        i64::try_from(record.progress.downloaded_bytes()).map_err(|_| DownloadError::Storage)?;
    let total = record
        .progress
        .total_bytes()
        .map(i64::try_from)
        .transpose()
        .map_err(|_| DownloadError::Storage)?;
    Ok((downloaded, total))
}

fn decode_record(row: SqliteRow) -> Result<DownloadRecord, StorageError> {
    let status = match row.try_get::<&str, _>("status")? {
        "queued" => DownloadStatus::Queued,
        "downloading" => DownloadStatus::Downloading,
        "completed" => DownloadStatus::Completed,
        "failed" => DownloadStatus::Failed,
        _ => return Err(StorageError::InvalidData),
    };
    let downloaded = u64::try_from(row.try_get::<i64, _>("downloaded_bytes")?)
        .map_err(|_| StorageError::InvalidData)?;
    let total = row
        .try_get::<Option<i64>, _>("total_bytes")?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| StorageError::InvalidData)?;
    Ok(DownloadRecord {
        key: DownloadKey {
            channel_id: row.try_get("channel_id")?,
            message_id: row.try_get("message_id")?,
            document_id: row.try_get("document_id")?,
        },
        file_name: row.try_get("file_name")?,
        local_path: row.try_get("local_path")?,
        status,
        progress: DownloadProgress::new(downloaded, total)
            .map_err(|_| StorageError::InvalidData)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Result<DownloadRecord, tgbooks_core::InvalidProgress> {
        Ok(DownloadRecord {
            key: DownloadKey {
                channel_id: 42,
                message_id: 7,
                document_id: 99,
            },
            file_name: "Книга.epub".into(),
            local_path: "books/Книга.epub".into(),
            status: DownloadStatus::Completed,
            progress: DownloadProgress::new(100, Some(100))?,
        })
    }

    #[tokio::test]
    async fn migrations_and_history_survive_reopening() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("История книг.sqlite3");
        let storage = Storage::open(&path).await?;
        assert!(storage.list_downloads().await?.is_empty());
        let expected = record()?;
        assert!(storage.insert_download(&expected).await?);
        assert!(!storage.insert_download(&expected).await?);
        assert!(storage.contains(expected.key).await?);
        storage.close().await;

        // Reopening reruns migrations safely and reads actual disk-backed state.
        let reopened = Storage::open(&path).await?;
        assert_eq!(reopened.list_downloads().await?, vec![expected]);
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn deduplication_is_scoped_to_telegram_identifiers()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        let first = record()?;
        let mut other_channel = first.clone();
        other_channel.key.channel_id = 43;
        assert!(storage.insert_download(&first).await?);
        assert!(storage.insert_download(&other_channel).await?);
        assert_eq!(storage.list_downloads().await?.len(), 2);
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn atomic_claim_retries_only_failed_records_and_completion_is_immutable()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        let mut job = record()?;
        job.status = DownloadStatus::Queued;
        job.progress = DownloadProgress::new(0, Some(100))?;
        let (first, second) = tokio::join!(storage.claim(&job), storage.claim(&job));
        assert_ne!(first?, second?);
        job.status = DownloadStatus::Downloading;
        job.progress = DownloadProgress::new(25, Some(100))?;
        storage.update(&job).await?;
        let mut stale = job.clone();
        stale.progress = DownloadProgress::new(10, Some(100))?;
        assert_eq!(storage.update(&stale).await, Err(DownloadError::Storage));
        storage.recover_interrupted().await?;
        assert_eq!(
            storage.list_downloads().await?[0].status,
            DownloadStatus::Failed
        );
        job.status = DownloadStatus::Queued;
        job.progress = DownloadProgress::new(0, Some(100))?;
        assert!(storage.claim(&job).await?);
        job.status = DownloadStatus::Completed;
        job.progress = DownloadProgress::new(100, Some(100))?;
        storage.update(&job).await?;
        let completed = job.clone();
        job.status = DownloadStatus::Failed;
        assert_eq!(storage.update(&job).await, Err(DownloadError::Storage));
        storage.recover_interrupted().await?;
        assert_eq!(storage.list_downloads().await?, vec![completed]);
        job.status = DownloadStatus::Queued;
        job.progress = DownloadProgress::new(0, Some(100))?;
        assert!(!storage.claim(&job).await?);
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn oversized_values_fail_without_truncation() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        let mut invalid = record()?;
        invalid.progress = DownloadProgress::new(u64::MAX, Some(u64::MAX))?;
        assert!(matches!(
            storage.insert_download(&invalid).await,
            Err(StorageError::InvalidData)
        ));
        assert!(storage.list_downloads().await?.is_empty());
        storage.close().await;
        Ok(())
    }
}
