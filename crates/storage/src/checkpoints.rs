use sqlx::Row;
use tgbooks_core::{DownloadCheckpoint, DownloadError, DownloadRecord};

use super::Storage;

impl Storage {
    pub(super) async fn read_checkpoint(
        &self,
        record: &DownloadRecord,
    ) -> Result<Option<DownloadCheckpoint>, DownloadError> {
        let row = sqlx::query(
            "SELECT part_path, file_name, total_bytes, downloaded_bytes, sha256, finished
             FROM download_checkpoints
             WHERE channel_id = ? AND message_id = ? AND document_id = ? AND local_path = ?",
        )
        .bind(record.key.channel_id)
        .bind(record.key.message_id)
        .bind(record.key.document_id)
        .bind(&record.local_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| DownloadError::Storage)?;
        row.map(|row| {
            let read = || -> Result<DownloadCheckpoint, Box<dyn std::error::Error>> {
                Ok(DownloadCheckpoint {
                    part_path: row.try_get::<String, _>("part_path")?.into(),
                    file_name: row.try_get("file_name")?,
                    total_bytes: row
                        .try_get::<Option<i64>, _>("total_bytes")?
                        .map(u64::try_from)
                        .transpose()?,
                    downloaded_bytes: u64::try_from(row.try_get::<i64, _>("downloaded_bytes")?)?,
                    sha256: row
                        .try_get::<Vec<u8>, _>("sha256")?
                        .try_into()
                        .map_err(|_| "invalid checkpoint digest")?,
                    finished: row.try_get("finished")?,
                })
            };
            read().map_err(|_| DownloadError::Storage)
        })
        .transpose()
    }

    pub(super) async fn write_checkpoint(
        &self,
        record: &DownloadRecord,
        checkpoint: &DownloadCheckpoint,
    ) -> Result<(), DownloadError> {
        let bytes =
            i64::try_from(checkpoint.downloaded_bytes).map_err(|_| DownloadError::Storage)?;
        let total = checkpoint
            .total_bytes
            .map(i64::try_from)
            .transpose()
            .map_err(|_| DownloadError::Storage)?;
        let part = checkpoint
            .part_path
            .to_str()
            .ok_or(DownloadError::Destination)?;
        sqlx::query(
            "INSERT INTO download_checkpoints
             (channel_id, message_id, document_id, local_path, part_path, file_name, total_bytes, downloaded_bytes, sha256, finished)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (channel_id, message_id, document_id, local_path) DO UPDATE SET
             part_path = excluded.part_path, file_name = excluded.file_name,
             total_bytes = excluded.total_bytes, downloaded_bytes = excluded.downloaded_bytes,
             sha256 = excluded.sha256, finished = excluded.finished",
        )
        .bind(record.key.channel_id).bind(record.key.message_id).bind(record.key.document_id)
        .bind(&record.local_path).bind(part).bind(&checkpoint.file_name).bind(total).bind(bytes)
        .bind(checkpoint.sha256.as_slice())
        .bind(checkpoint.finished)
        .execute(&self.pool).await.map_err(|_| DownloadError::Storage)?;
        Ok(())
    }

    pub(super) async fn delete_checkpoint(
        &self,
        record: &DownloadRecord,
    ) -> Result<(), DownloadError> {
        sqlx::query(
            "DELETE FROM download_checkpoints
             WHERE channel_id = ? AND message_id = ? AND document_id = ? AND local_path = ?",
        )
        .bind(record.key.channel_id)
        .bind(record.key.message_id)
        .bind(record.key.document_id)
        .bind(&record.local_path)
        .execute(&self.pool)
        .await
        .map_err(|_| DownloadError::Storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tgbooks_core::{
        DownloadHistory, DownloadKey, DownloadProgress, DownloadRepository, DownloadStatus,
    };

    fn record() -> Result<DownloadRecord, tgbooks_core::InvalidProgress> {
        Ok(DownloadRecord {
            key: DownloadKey {
                channel_id: -1,
                message_id: 2,
                document_id: 3,
            },
            file_name: "book.epub".into(),
            local_path: "/books/book.epub".into(),
            source_title: None,
            added_at: 0,
            status: DownloadStatus::Downloading,
            progress: DownloadProgress::new(3, Some(3))?,
        })
    }

    fn checkpoint() -> DownloadCheckpoint {
        DownloadCheckpoint {
            part_path: "/books/.tgbooks-test.part".into(),
            file_name: "book.epub".into(),
            total_bytes: Some(3),
            downloaded_bytes: 3,
            sha256: [7; 32],
            finished: true,
        }
    }

    #[tokio::test]
    async fn completion_and_checkpoint_deletion_are_one_transaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let storage = Storage::open(&root.path().join("downloads.sqlite3")).await?;
        let mut record = record()?;
        storage.insert_download(&record).await?;
        storage.save_checkpoint(&record, &checkpoint()).await?;
        sqlx::query(
            "CREATE TRIGGER reject_checkpoint_delete BEFORE DELETE ON download_checkpoints
            BEGIN SELECT RAISE(FAIL, 'test failure'); END",
        )
        .execute(&storage.pool)
        .await?;
        record.status = DownloadStatus::Completed;
        assert_eq!(storage.update(&record).await, Err(DownloadError::Storage));
        assert_eq!(
            storage.list_downloads().await?[0].status,
            DownloadStatus::Downloading
        );
        assert_eq!(storage.checkpoint(&record).await?, Some(checkpoint()));
        sqlx::query("DROP TRIGGER reject_checkpoint_delete")
            .execute(&storage.pool)
            .await?;
        storage.update(&record).await?;
        assert_eq!(storage.list_downloads().await?, vec![record.clone()]);
        assert!(storage.checkpoint(&record).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn invalid_offsets_do_not_replace_the_last_valid_checkpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let storage = Storage::open(&root.path().join("downloads.sqlite3")).await?;
        let record = record()?;
        storage.insert_download(&record).await?;
        storage.save_checkpoint(&record, &checkpoint()).await?;
        for bytes in [4, u64::MAX] {
            let mut invalid = checkpoint();
            invalid.downloaded_bytes = bytes;
            assert_eq!(
                storage.save_checkpoint(&record, &invalid).await,
                Err(DownloadError::Storage)
            );
            assert_eq!(storage.checkpoint(&record).await?, Some(checkpoint()));
        }
        Ok(())
    }
}
