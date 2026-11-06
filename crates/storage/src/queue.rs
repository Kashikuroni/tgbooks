use std::collections::HashSet;

use sqlx::{Row, sqlite::SqliteRow};
use tgbooks_core::{
    BookAttachment, BookFormat, DownloadError, DownloadKey, MAX_PENDING_DOWNLOADS, QueueStore,
    QueuedDownload,
};

use super::Storage;

impl QueueStore for Storage {
    async fn load_queue(&self, account: i64) -> Result<Vec<QueuedDownload>, DownloadError> {
        if account <= 0 {
            return Err(DownloadError::Storage);
        }
        let rows = sqlx::query(
            "SELECT q.channel_id, q.message_id, q.document_id, q.file_name, q.total_bytes,
                    q.format, q.directory, q.interrupted, q.redownload
             FROM download_queue q
             WHERE q.account_id = ?
               AND (q.redownload = 1 OR NOT EXISTS (
                   SELECT 1 FROM downloads d WHERE d.channel_id = q.channel_id
                     AND d.message_id = q.message_id AND d.document_id = q.document_id
                     AND d.status = 'completed'))
             ORDER BY q.position LIMIT ?",
        )
        .bind(account)
        .bind((MAX_PENDING_DOWNLOADS + 2) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DownloadError::Storage)?;
        if rows.len() > MAX_PENDING_DOWNLOADS + 1 {
            return Err(DownloadError::Storage);
        }
        rows.into_iter().map(decode).collect()
    }

    async fn save_queue(&self, account: i64, jobs: &[QueuedDownload]) -> Result<(), DownloadError> {
        if account <= 0 || jobs.len() > MAX_PENDING_DOWNLOADS + 1 {
            return Err(DownloadError::Storage);
        }
        let mut keys = HashSet::new();
        for job in jobs {
            validate(job)?;
            if !keys.insert(job.book.key) {
                return Err(DownloadError::Storage);
            }
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| DownloadError::Storage)?;
        sqlx::query("DELETE FROM download_queue WHERE account_id = ?")
            .bind(account)
            .execute(&mut *transaction)
            .await
            .map_err(|_| DownloadError::Storage)?;
        for (position, job) in jobs.iter().enumerate() {
            let size = job
                .book
                .size_bytes
                .map(i64::try_from)
                .transpose()
                .map_err(|_| DownloadError::Storage)?;
            let directory = job.directory.to_str().ok_or(DownloadError::Destination)?;
            sqlx::query(
                "INSERT INTO download_queue
                 (account_id, position, channel_id, message_id, document_id, file_name,
                  total_bytes, format, directory, interrupted, redownload)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(account)
            .bind(position as i64)
            .bind(job.book.key.channel_id)
            .bind(job.book.key.message_id)
            .bind(job.book.key.document_id)
            .bind(&job.book.file_name)
            .bind(size)
            .bind(job.book.format.extension())
            .bind(directory)
            .bind(job.interrupted)
            .bind(job.redownload)
            .execute(&mut *transaction)
            .await
            .map_err(|_| DownloadError::Storage)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| DownloadError::Storage)
    }
}

fn validate(job: &QueuedDownload) -> Result<(), DownloadError> {
    if !job.directory.is_absolute() || job.directory.to_str().is_none() {
        return Err(DownloadError::Destination);
    }
    if BookFormat::from_file_name(&job.book.file_name) != Some(job.book.format)
        || job
            .book
            .size_bytes
            .is_some_and(|size| i64::try_from(size).is_err())
    {
        return Err(DownloadError::Storage);
    }
    Ok(())
}

fn decode(row: SqliteRow) -> Result<QueuedDownload, DownloadError> {
    let read = || -> Result<QueuedDownload, sqlx::Error> {
        let format: String = row.try_get("format")?;
        let size: Option<i64> = row.try_get("total_bytes")?;
        let total_bytes = size
            .map(u64::try_from)
            .transpose()
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let format = BookFormat::ALL
            .into_iter()
            .find(|value| value.extension() == format)
            .ok_or_else(|| sqlx::Error::Decode("invalid book format".into()))?;
        Ok(QueuedDownload {
            book: BookAttachment {
                key: DownloadKey {
                    channel_id: row.try_get("channel_id")?,
                    message_id: row.try_get("message_id")?,
                    document_id: row.try_get("document_id")?,
                },
                file_name: row.try_get("file_name")?,
                size_bytes: total_bytes,
                format,
            },
            directory: row.try_get::<String, _>("directory")?.into(),
            interrupted: row.try_get("interrupted")?,
            redownload: row.try_get("redownload")?,
        })
    };
    let job = read().map_err(|_| DownloadError::Storage)?;
    validate(&job).map_err(|_| DownloadError::Storage)?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tgbooks_core::{DownloadProgress, DownloadRecord, DownloadStatus};

    fn job(index: i32) -> QueuedDownload {
        QueuedDownload {
            book: BookAttachment {
                key: DownloadKey {
                    channel_id: -1000000000001,
                    message_id: index,
                    document_id: i64::from(index) + 100,
                },
                file_name: format!("Книга 世界 {index}.epub"),
                size_bytes: (index != 2).then_some(3),
                format: BookFormat::Epub,
            },
            directory: std::env::temp_dir().join(format!("Книги {index}")),
            interrupted: index == 1,
            redownload: false,
        }
    }

    #[tokio::test]
    async fn upgrade_from_settings_schema_preserves_history_preferences_and_queue()
    -> Result<(), Box<dyn std::error::Error>> {
        use tgbooks_core::{DownloadHistory as _, SettingsStore as _};
        let temporary = tempfile::tempdir()?;
        let old_migrations = temporary.path().join("old-migrations");
        tokio::fs::create_dir(&old_migrations).await?;
        for (name, sql) in [
            (
                "0001_downloads.sql",
                include_str!("../migrations/0001_downloads.sql"),
            ),
            (
                "0002_download_queue.sql",
                include_str!("../migrations/0002_download_queue.sql"),
            ),
            (
                "0003_download_source.sql",
                include_str!("../migrations/0003_download_source.sql"),
            ),
            (
                "0004_app_settings.sql",
                include_str!("../migrations/0004_app_settings.sql"),
            ),
        ] {
            tokio::fs::write(old_migrations.join(name), sql).await?;
        }
        let path = temporary.path().join("downloads.sqlite3");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await?;
        sqlx::migrate::Migrator::new(old_migrations.as_path())
            .await?
            .run(&pool)
            .await?;
        sqlx::query("INSERT INTO downloads (channel_id, message_id, document_id, file_name, local_path, status, downloaded_bytes, total_bytes, created_at, source_title)
            VALUES (1, 2, 3, 'Книга.epub', 'old/book.epub', 'completed', 3, 3, 123, 'Канал')")
            .execute(&pool).await?;
        sqlx::query("INSERT INTO app_settings VALUES (1, ?, 3)")
            .bind(temporary.path().to_str().ok_or("non-UTF8 test path")?)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO download_queue (account_id, position, channel_id, message_id, document_id, file_name, total_bytes, format, directory, interrupted)
            VALUES (1, 0, 4, 5, 6, 'queue.epub', 3, 'epub', ?, 1)")
            .bind(temporary.path().to_str().ok_or("non-UTF8 test path")?).execute(&pool).await?;
        pool.close().await;
        let storage = Storage::open(&path).await?;
        let records = storage.list_downloads().await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].local_path, "old/book.epub");
        assert_eq!(records[0].source_title.as_deref(), Some("Канал"));
        assert_eq!(records[0].added_at, 123);
        let preferences = storage
            .load_settings()
            .await
            .map_err(|_| "invalid preferences")?
            .ok_or("missing preferences")?;
        assert_eq!(preferences.download_directory, temporary.path());
        assert_eq!(preferences.formats.mask(), 3);
        let jobs = storage.load_queue(1).await?;
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].interrupted);
        assert!(!jobs[0].redownload);
        assert_eq!(jobs[0].book.key.message_id, 5);
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn repeat_completion_and_intent_removal_are_atomic()
    -> Result<(), Box<dyn std::error::Error>> {
        use tgbooks_core::{DownloadHistory as _, DownloadRepository as _};
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("downloads.sqlite3");
        let storage = Storage::open(&path).await?;
        let mut repeat = job(1);
        repeat.redownload = true;
        let original = DownloadRecord {
            key: repeat.book.key,
            file_name: repeat.book.file_name.clone(),
            local_path: "old/book.epub".into(),
            source_title: Some("Канал".into()),
            added_at: 123,
            status: DownloadStatus::Completed,
            progress: DownloadProgress::new(3, Some(3))?,
        };
        storage.insert_download(&original).await?;
        storage.save_queue(1, &[repeat.clone()]).await?;
        assert_eq!(storage.load_queue(1).await?, vec![repeat]);
        let mut copy = original.clone();
        copy.local_path = "new/book.epub".into();
        copy.status = DownloadStatus::Queued;
        copy.progress = DownloadProgress::new(0, Some(3))?;
        assert!(!storage.claim(&copy).await?);
        assert!(storage.claim_again(&copy).await?);
        let mut concurrent = copy.clone();
        concurrent.local_path = "third/book.epub".into();
        assert!(!storage.claim_again(&concurrent).await?);
        copy.status = DownloadStatus::Completed;
        copy.progress = DownloadProgress::new(3, Some(3))?;
        sqlx::query("CREATE TRIGGER reject_intent_delete BEFORE DELETE ON download_queue BEGIN SELECT RAISE(FAIL, 'test'); END")
            .execute(&storage.pool).await?;
        assert_eq!(storage.update(&copy).await, Err(DownloadError::Storage));
        assert_eq!(storage.load_queue(1).await?.len(), 1);
        assert!(
            storage
                .list_downloads()
                .await?
                .iter()
                .any(|record| record.local_path == copy.local_path
                    && record.status == DownloadStatus::Queued)
        );
        sqlx::query("DROP TRIGGER reject_intent_delete")
            .execute(&storage.pool)
            .await?;
        storage.update(&copy).await?;
        assert!(storage.load_queue(1).await?.is_empty());
        storage.close().await;
        let reopened = Storage::open(&path).await?;
        let history = reopened.list_downloads().await?;
        assert_eq!(history.len(), 2);
        assert!(history.contains(&original));
        assert!(history.contains(&copy));
        assert!(reopened.load_queue(1).await?.is_empty());
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn upgrading_existing_history_applies_only_the_new_migration()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let old_migrations = temporary.path().join("old-migrations");
        tokio::fs::create_dir(&old_migrations).await?;
        tokio::fs::write(
            old_migrations.join("0001_downloads.sql"),
            include_str!("../migrations/0001_downloads.sql"),
        )
        .await?;
        let path = temporary.path().join("downloads.sqlite3");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await?;
        sqlx::migrate::Migrator::new(old_migrations.as_path())
            .await?
            .run(&pool)
            .await?;
        let original = DownloadRecord {
            key: job(1).book.key,
            file_name: "old.epub".into(),
            source_title: None,
            added_at: 0,
            local_path: "old.epub".into(),
            status: DownloadStatus::Completed,
            progress: DownloadProgress::new(3, Some(3))?,
        };
        let legacy = Storage { pool };
        // Exercise the actual v1 schema, before source_title exists.
        sqlx::query("INSERT INTO downloads (channel_id, message_id, document_id, file_name, local_path, status, downloaded_bytes, total_bytes, created_at)
            VALUES (?, ?, ?, 'old.epub', 'old.epub', 'completed', 3, 3, 0)")
            .bind(original.key.channel_id).bind(original.key.message_id).bind(original.key.document_id)
            .execute(&legacy.pool).await?;
        legacy.close().await;
        let upgraded = Storage::open(&path).await?;
        use tgbooks_core::DownloadHistory as _;
        assert_eq!(upgraded.list_downloads().await?, vec![original]);
        assert!(upgraded.load_queue(1).await?.is_empty());
        upgraded.save_queue(1, &[job(2)]).await?;
        assert_eq!(upgraded.load_queue(1).await?, vec![job(2)]);
        upgraded.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn ordered_intent_and_account_isolation_survive_reopening()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("downloads.sqlite3");
        let storage = Storage::open(&path).await?;
        let jobs = vec![job(3), job(1), job(2)];
        storage.save_queue(1, &jobs).await?;
        storage.save_queue(2, &[job(9)]).await?;
        storage.close().await;
        let reopened = Storage::open(&path).await?;
        assert_eq!(reopened.load_queue(1).await?, jobs);
        assert_eq!(reopened.load_queue(2).await?, vec![job(9)]);
        assert!(reopened.load_queue(3).await?.is_empty());
        reopened.save_queue(1, &[job(1), job(2)]).await?;
        assert_eq!(reopened.load_queue(1).await?, vec![job(1), job(2)]);
        reopened.save_queue(1, &[]).await?;
        assert!(reopened.load_queue(1).await?.is_empty());
        assert_eq!(reopened.load_queue(2).await?, vec![job(9)]);
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn failed_replace_rolls_back_deletion_and_partial_insertions()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        storage.save_queue(1, &[job(1)]).await?;
        sqlx::query(
            "CREATE TRIGGER reject_queue_job BEFORE INSERT ON download_queue
             WHEN NEW.message_id = 3 BEGIN SELECT RAISE(FAIL, 'test write failure'); END",
        )
        .execute(&storage.pool)
        .await?;
        assert_eq!(
            storage.save_queue(1, &[job(2), job(3)]).await,
            Err(DownloadError::Storage)
        );
        assert_eq!(storage.load_queue(1).await?, vec![job(1)]);
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn completed_history_wins_over_stale_active_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        let completed = job(1);
        storage.save_queue(1, &[completed.clone(), job(2)]).await?;
        storage
            .insert_download(&DownloadRecord {
                key: completed.book.key,
                file_name: completed.book.file_name,
                source_title: None,
                added_at: 0,
                local_path: "book.epub".into(),
                status: DownloadStatus::Completed,
                progress: DownloadProgress::new(3, Some(3))?,
            })
            .await?;
        assert_eq!(storage.load_queue(1).await?, vec![job(2)]);
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_snapshots_and_corruption_fail_without_erasing_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        storage.save_queue(1, &[job(1)]).await?;
        assert_eq!(
            storage.save_queue(1, &[job(2), job(2)]).await,
            Err(DownloadError::Storage)
        );
        let mut invalid = job(2);
        invalid.directory = "relative".into();
        assert_eq!(
            storage.save_queue(1, &[invalid]).await,
            Err(DownloadError::Destination)
        );
        let mut oversized = job(2);
        oversized.book.size_bytes = Some(u64::MAX);
        assert_eq!(
            storage.save_queue(1, &[oversized]).await,
            Err(DownloadError::Storage)
        );
        assert_eq!(storage.load_queue(1).await?, vec![job(1)]);
        sqlx::query("UPDATE download_queue SET directory = 'relative' WHERE account_id = 1")
            .execute(&storage.pool)
            .await?;
        assert_eq!(storage.load_queue(1).await, Err(DownloadError::Storage));
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM download_queue WHERE account_id = 1")
                .fetch_one(&storage.pool)
                .await?;
        assert_eq!(count, 1);
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_allows_one_interrupted_job_plus_the_full_waiting_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        let jobs: Vec<_> = (0..=MAX_PENDING_DOWNLOADS as i32).map(job).collect();
        storage.save_queue(1, &jobs).await?;
        assert_eq!(storage.load_queue(1).await?, jobs);
        let mut excessive = jobs.clone();
        excessive.push(job(MAX_PENDING_DOWNLOADS as i32 + 1));
        assert_eq!(
            storage.save_queue(1, &excessive).await,
            Err(DownloadError::Storage)
        );
        assert_eq!(storage.load_queue(1).await?, jobs);
        storage.close().await;
        Ok(())
    }
}
