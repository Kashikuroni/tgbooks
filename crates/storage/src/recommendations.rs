use super::Storage;
use sqlx::Row;
use tgbooks_core::{
    BookAttachment, BookFormat, DownloadKey, MonitorCursor, Recommendation,
    RecommendationError as Error, RecommendationStatus as Status, RecommendationStore,
};

fn status(value: Status) -> &'static str {
    match value {
        Status::Unread => "unread",
        Status::Read => "read",
        Status::Hidden => "hidden",
    }
}

impl RecommendationStore for Storage {
    async fn load_monitoring_enabled(&self, account: i64) -> Result<bool, Error> {
        if account <= 0 {
            return Err(Error::Unauthorized);
        }
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT enabled FROM monitor_preferences WHERE account_id = ?",
        )
        .bind(account)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| Error::Storage)?
        .unwrap_or(false))
    }

    async fn save_monitoring_enabled(&self, account: i64, enabled: bool) -> Result<(), Error> {
        if account <= 0 {
            return Err(Error::Unauthorized);
        }
        sqlx::query(
            "INSERT INTO monitor_preferences (account_id, enabled) VALUES (?, ?)
            ON CONFLICT (account_id) DO UPDATE SET enabled = excluded.enabled",
        )
        .bind(account)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(|_| Error::Storage)?;
        Ok(())
    }

    async fn load_recommendations(&self, account: i64) -> Result<Vec<Recommendation>, Error> {
        if account <= 0 {
            return Err(Error::Unauthorized);
        }
        let rows = sqlx::query(
            "SELECT r.* FROM recommendations r WHERE r.account_id = ? AND r.status != 'hidden'
             AND NOT EXISTS (SELECT 1 FROM downloads d WHERE d.channel_id = r.channel_id
                AND d.message_id = r.message_id AND d.document_id = r.document_id AND d.status = 'completed')
             ORDER BY r.message_date DESC, r.channel_id, r.message_id DESC, r.document_id",
        ).bind(account).fetch_all(&self.pool).await.map_err(|_| Error::Storage)?;
        rows.into_iter()
            .map(|row| {
                let read = || -> Result<Recommendation, sqlx::Error> {
                    let format: String = row.try_get("format")?;
                    let format = BookFormat::ALL
                        .into_iter()
                        .find(|value| value.extension() == format)
                        .ok_or_else(|| sqlx::Error::Decode("invalid format".into()))?;
                    let size: Option<i64> = row.try_get("total_bytes")?;
                    let size_bytes = size
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                    let disposition: String = row.try_get("status")?;
                    let disposition = match disposition.as_str() {
                        "unread" => Status::Unread,
                        "read" => Status::Read,
                        _ => {
                            return Err(sqlx::Error::Decode("invalid recommendation state".into()));
                        }
                    };
                    Ok(Recommendation {
                        book: BookAttachment {
                            key: DownloadKey {
                                channel_id: row.try_get("channel_id")?,
                                message_id: row.try_get("message_id")?,
                                document_id: row.try_get("document_id")?,
                            },
                            file_name: row.try_get("file_name")?,
                            format,
                            size_bytes,
                        },
                        source_title: row.try_get("source_title")?,
                        message_date: row.try_get("message_date")?,
                        status: disposition,
                    })
                };
                read().map_err(|_| Error::Storage)
            })
            .collect()
    }

    async fn load_monitor_cursor(
        &self,
        account: i64,
        channel: i64,
    ) -> Result<Option<MonitorCursor>, Error> {
        if account <= 0 {
            return Err(Error::Unauthorized);
        }
        let row = sqlx::query("SELECT after_id, before_id, target_id FROM monitor_cursors WHERE account_id = ? AND channel_id = ?")
            .bind(account).bind(channel).fetch_optional(&self.pool).await.map_err(|_| Error::Storage)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let read = || -> Result<MonitorCursor, sqlx::Error> {
            Ok(MonitorCursor {
                after: row.try_get("after_id")?,
                before: row.try_get("before_id")?,
                target: row.try_get("target_id")?,
            })
        };
        let cursor = read().map_err(|_| Error::Storage)?;
        if !cursor.valid() {
            return Err(Error::Storage);
        }
        Ok(Some(cursor))
    }

    async fn commit_recommendations(
        &self,
        account: i64,
        channel: i64,
        cursor: MonitorCursor,
        items: &[Recommendation],
    ) -> Result<Vec<DownloadKey>, Error> {
        if account <= 0 {
            return Err(Error::Unauthorized);
        }
        if !cursor.valid() || items.len() > tgbooks_core::SCAN_PAGE_SIZE {
            return Err(Error::Storage);
        }
        let mut transaction = self.pool.begin().await.map_err(|_| Error::Storage)?;
        let mut inserted = Vec::new();
        for item in items {
            if item.book.key.channel_id != channel
                || item.book.key.message_id <= 0
                || BookFormat::from_file_name(&item.book.file_name) != Some(item.book.format)
            {
                return Err(Error::Storage);
            }
            let size = item
                .book
                .size_bytes
                .map(i64::try_from)
                .transpose()
                .map_err(|_| Error::Storage)?;
            let result = sqlx::query("INSERT INTO recommendations (account_id, channel_id, message_id, document_id,
                file_name, format, total_bytes, source_title, message_date, status)
                SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, 'unread'
                WHERE NOT EXISTS (SELECT 1 FROM downloads WHERE channel_id = ? AND message_id = ? AND document_id = ? AND status = 'completed')
                ON CONFLICT (account_id, channel_id, message_id, document_id) DO NOTHING")
                .bind(account).bind(channel).bind(item.book.key.message_id).bind(item.book.key.document_id)
                .bind(&item.book.file_name).bind(item.book.format.extension()).bind(size).bind(&item.source_title).bind(item.message_date)
                .bind(channel).bind(item.book.key.message_id).bind(item.book.key.document_id)
                .execute(&mut *transaction).await.map_err(|_| Error::Storage)?;
            if result.rows_affected() == 1 {
                inserted.push(item.book.key);
            }
        }
        sqlx::query("INSERT INTO monitor_cursors (account_id, channel_id, after_id, before_id, target_id)
            VALUES (?, ?, ?, ?, ?) ON CONFLICT (account_id, channel_id) DO UPDATE SET
            after_id = excluded.after_id, before_id = excluded.before_id, target_id = excluded.target_id")
            .bind(account).bind(channel).bind(cursor.after).bind(cursor.before).bind(cursor.target)
            .execute(&mut *transaction).await.map_err(|_| Error::Storage)?;
        transaction.commit().await.map_err(|_| Error::Storage)?;
        Ok(inserted)
    }

    async fn set_recommendation_status(
        &self,
        account: i64,
        keys: &[DownloadKey],
        disposition: Status,
    ) -> Result<(), Error> {
        if account <= 0 {
            return Err(Error::Unauthorized);
        }
        if keys.len() > tgbooks_core::MAX_PENDING_DOWNLOADS {
            return Err(Error::Storage);
        }
        let mut transaction = self.pool.begin().await.map_err(|_| Error::Storage)?;
        for key in keys {
            sqlx::query(
                "UPDATE recommendations SET status = ? WHERE account_id = ?
                AND channel_id = ? AND message_id = ? AND document_id = ? AND status != 'hidden'",
            )
            .bind(status(disposition))
            .bind(account)
            .bind(key.channel_id)
            .bind(key.message_id)
            .bind(key.document_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| Error::Storage)?;
        }
        transaction.commit().await.map_err(|_| Error::Storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tgbooks_core::{
        ChannelSummary, DownloadProgress, DownloadRecord, DownloadStatus, SourceStore,
    };

    fn source() -> ChannelSummary {
        ChannelSummary {
            id: -1,
            title: "Books 世界".into(),
            username: None,
            protected: false,
        }
    }
    fn item(message_id: i32) -> Recommendation {
        Recommendation {
            book: BookAttachment {
                key: DownloadKey {
                    channel_id: -1,
                    message_id,
                    document_id: i64::from(message_id),
                },
                file_name: format!("Книга-{message_id}.epub"),
                size_bytes: Some(3),
                format: BookFormat::Epub,
            },
            source_title: source().title,
            message_date: 1_794_000_000 + i64::from(message_id),
            status: Status::Unread,
        }
    }
    fn cursor(after: i32) -> MonitorCursor {
        MonitorCursor {
            after,
            before: None,
            target: None,
        }
    }

    #[tokio::test]
    async fn only_new_rows_produce_notification_candidates_and_monitor_intent_survives_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("downloads.sqlite3");
        let storage = Storage::open(&path).await?;
        assert!(!storage.load_monitoring_enabled(1).await?);
        assert_eq!(storage.set_source(1, &source(), true).await, Ok(()));
        storage.save_monitoring_enabled(1, true).await?;
        let first = storage
            .commit_recommendations(1, -1, cursor(101), &[item(101)])
            .await?;
        assert_eq!(first, vec![item(101).book.key]);
        assert!(
            storage
                .commit_recommendations(1, -1, cursor(101), &[item(101)])
                .await?
                .is_empty()
        );
        storage.close().await;
        let reopened = Storage::open(&path).await?;
        assert!(reopened.load_monitoring_enabled(1).await?);
        assert!(!reopened.load_monitoring_enabled(2).await?);
        assert!(
            reopened
                .commit_recommendations(1, -1, cursor(101), &[item(101)])
                .await?
                .is_empty()
        );
        assert_eq!(reopened.load_recommendations(1).await?.len(), 1);
        reopened.save_monitoring_enabled(1, false).await?;
        assert!(!reopened.load_monitoring_enabled(1).await?);
        sqlx::query("CREATE TRIGGER reject_monitor_setting BEFORE UPDATE ON monitor_preferences BEGIN SELECT RAISE(FAIL, 'test'); END")
            .execute(&reopened.pool).await?;
        assert_eq!(
            reopened.save_monitoring_enabled(1, true).await,
            Err(Error::Storage)
        );
        assert!(!reopened.load_monitoring_enabled(1).await?);
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn inbox_dispositions_and_checkpoints_survive_restart_without_cross_account_leaks()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("downloads.sqlite3");
        let storage = Storage::open(&path).await?;
        assert_eq!(storage.set_source(1, &source(), true).await, Ok(()));
        assert_eq!(storage.set_source(2, &source(), true).await, Ok(()));
        let items = vec![item(101), item(102), item(103)];
        storage
            .commit_recommendations(1, -1, cursor(103), &items)
            .await?;
        storage
            .commit_recommendations(2, -1, cursor(103), &items)
            .await?;
        storage
            .set_recommendation_status(1, &[item(101).book.key], Status::Read)
            .await?;
        storage
            .set_recommendation_status(1, &[item(102).book.key], Status::Hidden)
            .await?;
        // Re-fetching a page must not resurrect a hidden item or mark read items unread.
        storage
            .commit_recommendations(1, -1, cursor(103), &items)
            .await?;
        storage.close().await;
        let reopened = Storage::open(&path).await?;
        let first = reopened.load_recommendations(1).await?;
        assert_eq!(first.len(), 2);
        assert!(
            first
                .iter()
                .any(|item| item.book.key.message_id == 101 && item.status == Status::Read)
        );
        assert_eq!(
            first
                .iter()
                .filter(|item| item.status == Status::Unread)
                .count(),
            1
        );
        assert_eq!(reopened.load_recommendations(2).await?.len(), 3);
        assert_eq!(
            reopened.load_monitor_cursor(1, -1).await?,
            Some(cursor(103))
        );
        assert_eq!(reopened.set_source(1, &source(), false).await, Ok(()));
        assert_eq!(reopened.load_monitor_cursor(1, -1).await?, None);
        assert_eq!(
            reopened.load_recommendations(1).await?,
            first,
            "removing a source must not erase accumulated findings"
        );
        assert_eq!(
            reopened.load_monitor_cursor(2, -1).await?,
            Some(cursor(103))
        );
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn findings_and_checkpoint_roll_back_together_on_either_write_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let storage = Storage::open(&root.path().join("downloads.sqlite3")).await?;
        assert_eq!(storage.set_source(1, &source(), true).await, Ok(()));
        storage
            .commit_recommendations(1, -1, cursor(100), &[])
            .await?;
        sqlx::query("CREATE TRIGGER reject_finding BEFORE INSERT ON recommendations WHEN NEW.message_id = 102 BEGIN SELECT RAISE(FAIL, 'test'); END")
            .execute(&storage.pool).await?;
        assert_eq!(
            storage
                .commit_recommendations(1, -1, cursor(102), &[item(101), item(102)])
                .await,
            Err(Error::Storage)
        );
        assert_eq!(storage.load_monitor_cursor(1, -1).await?, Some(cursor(100)));
        assert!(storage.load_recommendations(1).await?.is_empty());
        sqlx::query("DROP TRIGGER reject_finding")
            .execute(&storage.pool)
            .await?;
        sqlx::query("CREATE TRIGGER reject_cursor BEFORE UPDATE ON monitor_cursors BEGIN SELECT RAISE(FAIL, 'test'); END")
            .execute(&storage.pool).await?;
        assert_eq!(
            storage
                .commit_recommendations(1, -1, cursor(102), &[item(101)])
                .await,
            Err(Error::Storage)
        );
        assert!(storage.load_recommendations(1).await?.is_empty());
        assert_eq!(storage.load_monitor_cursor(1, -1).await?, Some(cursor(100)));
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn completed_books_are_not_recommended_and_stale_source_writes_are_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let storage = Storage::open(&root.path().join("downloads.sqlite3")).await?;
        assert_eq!(storage.set_source(1, &source(), true).await, Ok(()));
        let book = item(101);
        storage
            .insert_download(&DownloadRecord {
                key: book.book.key,
                file_name: book.book.file_name.clone(),
                local_path: "book.epub".into(),
                source_title: None,
                added_at: 0,
                status: DownloadStatus::Completed,
                progress: DownloadProgress::new(3, Some(3))?,
            })
            .await?;
        storage
            .commit_recommendations(1, -1, cursor(101), &[book])
            .await?;
        assert!(storage.load_recommendations(1).await?.is_empty());
        assert_eq!(storage.load_monitor_cursor(1, -1).await?, Some(cursor(101)));
        assert_eq!(storage.set_source(1, &source(), false).await, Ok(()));
        assert_eq!(
            storage
                .commit_recommendations(1, -1, cursor(102), &[item(102)])
                .await,
            Err(Error::Storage)
        );
        assert!(storage.load_recommendations(1).await?.is_empty());
        storage.close().await;
        Ok(())
    }
}
