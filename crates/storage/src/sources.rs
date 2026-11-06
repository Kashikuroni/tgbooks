use sqlx::Row;
use tgbooks_core::{ChannelSummary, SourceError, SourceStore};

use super::Storage;

impl SourceStore for Storage {
    async fn load_sources(&self, account: i64) -> Result<Vec<ChannelSummary>, SourceError> {
        if account <= 0 {
            return Err(SourceError::Unauthorized);
        }
        let rows = sqlx::query(
            "SELECT channel_id, title, username, protected FROM channel_sources
             WHERE account_id = ? ORDER BY title COLLATE NOCASE, channel_id",
        )
        .bind(account)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| SourceError::Storage)?;
        rows.into_iter()
            .map(|row| {
                let read = || -> Result<ChannelSummary, sqlx::Error> {
                    Ok(ChannelSummary {
                        id: row.try_get("channel_id")?,
                        title: row.try_get("title")?,
                        username: row.try_get("username")?,
                        protected: row.try_get("protected")?,
                    })
                };
                read().map_err(|_| SourceError::Storage)
            })
            .collect()
    }

    async fn set_source(
        &self,
        account: i64,
        channel: &ChannelSummary,
        enabled: bool,
    ) -> Result<(), SourceError> {
        if account <= 0 {
            return Err(SourceError::Unauthorized);
        }
        if channel.id == 0 {
            return Err(SourceError::Unavailable);
        }
        if enabled {
            sqlx::query(
                "INSERT INTO channel_sources (account_id, channel_id, title, username, protected)
                 VALUES (?, ?, ?, ?, ?) ON CONFLICT (account_id, channel_id)
                 DO UPDATE SET title = excluded.title, username = excluded.username, protected = excluded.protected",
            ).bind(account).bind(channel.id).bind(&channel.title).bind(&channel.username).bind(channel.protected)
                .execute(&self.pool).await.map_err(|_| SourceError::Storage)?;
        } else {
            sqlx::query("DELETE FROM channel_sources WHERE account_id = ? AND channel_id = ?")
                .bind(account)
                .bind(channel.id)
                .execute(&self.pool)
                .await
                .map_err(|_| SourceError::Storage)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> ChannelSummary {
        ChannelSummary {
            id: -1000000000001,
            title: "Книжная полка 世界".into(),
            username: Some("books".into()),
            protected: false,
        }
    }

    #[tokio::test]
    async fn sources_are_idempotent_account_scoped_and_survive_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("downloads.sqlite3");
        let storage = Storage::open(&path).await?;
        let original = channel();
        assert_eq!(storage.load_sources(1).await, Ok(Vec::new()));
        assert_eq!(storage.set_source(1, &original, true).await, Ok(()));
        assert_eq!(storage.set_source(1, &original, true).await, Ok(()));
        assert_eq!(storage.load_sources(1).await, Ok(vec![original.clone()]));
        assert_eq!(storage.load_sources(2).await, Ok(Vec::new()));
        let mut other = original.clone();
        other.title = "Другой аккаунт".into();
        other.protected = true;
        assert_eq!(storage.set_source(2, &other, true).await, Ok(()));
        storage.close().await;
        let reopened = Storage::open(&path).await?;
        assert_eq!(reopened.load_sources(1).await, Ok(vec![original.clone()]));
        assert_eq!(reopened.load_sources(2).await, Ok(vec![other.clone()]));
        assert_eq!(reopened.set_source(1, &original, false).await, Ok(()));
        assert_eq!(reopened.load_sources(1).await, Ok(Vec::new()));
        assert_eq!(reopened.load_sources(2).await, Ok(vec![other]));
        assert_eq!(
            reopened.load_sources(0).await,
            Err(SourceError::Unauthorized)
        );
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn failed_updates_and_deletions_keep_existing_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let storage = Storage::open(&temporary.path().join("downloads.sqlite3")).await?;
        let original = channel();
        assert_eq!(storage.set_source(1, &original, true).await, Ok(()));
        sqlx::query("CREATE TRIGGER reject_source_update BEFORE UPDATE ON channel_sources BEGIN SELECT RAISE(FAIL, 'test'); END")
            .execute(&storage.pool).await?;
        sqlx::query("CREATE TRIGGER reject_source_delete BEFORE DELETE ON channel_sources BEGIN SELECT RAISE(FAIL, 'test'); END")
            .execute(&storage.pool).await?;
        let mut renamed = original.clone();
        renamed.title = "Новое название".into();
        assert_eq!(
            storage.set_source(1, &renamed, true).await,
            Err(SourceError::Storage)
        );
        assert_eq!(
            storage.set_source(1, &original, false).await,
            Err(SourceError::Storage)
        );
        assert_eq!(storage.load_sources(1).await, Ok(vec![original]));
        storage.close().await;
        Ok(())
    }
}
