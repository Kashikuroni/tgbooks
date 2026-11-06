use sqlx::Row;
use tgbooks_core::{AppSettings, BookFormats, SettingsError, SettingsStore};

use super::Storage;

impl SettingsStore for Storage {
    async fn load_settings(&self) -> Result<Option<AppSettings>, SettingsError> {
        let row =
            sqlx::query("SELECT download_directory, formats, system_notifications FROM app_settings WHERE singleton = 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| SettingsError::Storage)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let path: String = row
            .try_get("download_directory")
            .map_err(|_| SettingsError::Storage)?;
        let mask: i64 = row.try_get("formats").map_err(|_| SettingsError::Storage)?;
        let formats = u8::try_from(mask)
            .ok()
            .and_then(BookFormats::from_mask)
            .ok_or(SettingsError::Storage)?;
        let download_directory = std::path::PathBuf::from(path);
        if !download_directory.is_absolute() {
            return Err(SettingsError::Storage);
        }
        // A disconnected volume must not make the entire app unusable at startup.
        // The downloader checks availability again when the job starts.
        Ok(Some(AppSettings {
            download_directory,
            formats,
            system_notifications: row
                .try_get("system_notifications")
                .map_err(|_| SettingsError::Storage)?,
        }))
    }

    async fn save_settings(&self, settings: &AppSettings) -> Result<AppSettings, SettingsError> {
        if !settings.download_directory.is_absolute() {
            return Err(SettingsError::Directory);
        }
        let directory = tokio::fs::canonicalize(&settings.download_directory)
            .await
            .map_err(|_| SettingsError::Directory)?;
        if !tokio::fs::metadata(&directory)
            .await
            .map_err(|_| SettingsError::Directory)?
            .is_dir()
        {
            return Err(SettingsError::Directory);
        }
        let probe_directory = directory.clone();
        tokio::task::spawn_blocking(move || {
            // Creating, then dropping, a unique temporary file checks actual write
            // access, unlike permissions().readonly() on Windows or ACL volumes.
            tempfile::Builder::new()
                .prefix(".tgbooks-check-")
                .tempfile_in(probe_directory)
                .map(drop)
        })
        .await
        .map_err(|_| SettingsError::Directory)?
        .map_err(|_| SettingsError::Directory)?;
        let text = directory.to_str().ok_or(SettingsError::Directory)?;
        sqlx::query("INSERT INTO app_settings (singleton, download_directory, formats, system_notifications) VALUES (1, ?, ?, ?)
            ON CONFLICT(singleton) DO UPDATE SET download_directory = excluded.download_directory, formats = excluded.formats,
            system_notifications = excluded.system_notifications")
            .bind(text).bind(i64::from(settings.formats.mask())).bind(settings.system_notifications)
            .execute(&self.pool).await.map_err(|_| SettingsError::Storage)?;
        Ok(AppSettings {
            download_directory: directory,
            formats: settings.formats,
            system_notifications: settings.system_notifications,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preferences_survive_restart_and_invalid_folders_never_replace_them()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let database = root.path().join("settings.sqlite3");
        let storage = Storage::open(&database).await?;
        assert_eq!(storage.load_settings().await, Ok(None));
        let Some(formats) = BookFormats::from_mask(3) else {
            panic!("formats");
        };
        let expected = storage
            .save_settings(&AppSettings {
                download_directory: root.path().to_path_buf(),
                formats,
                system_notifications: true,
            })
            .await;
        let Ok(expected) = expected else {
            panic!("valid directory was rejected");
        };
        for directory in [
            root.path().join("missing"),
            database.clone(),
            "relative".into(),
        ] {
            assert_eq!(
                storage.save_settings(&AppSettings::new(directory)).await,
                Err(SettingsError::Directory)
            );
            assert_eq!(storage.load_settings().await, Ok(Some(expected.clone())));
        }
        assert_eq!(
            std::fs::read_dir(root.path())?
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tgbooks-check-"))
                .count(),
            0
        );
        storage.close().await;
        let reopened = Storage::open(&database).await?;
        assert_eq!(reopened.load_settings().await, Ok(Some(expected)));
        reopened.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn malformed_saved_preferences_are_not_silently_replaced()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let storage = Storage::open(&root.path().join("settings.sqlite3")).await?;
        assert!(
            storage
                .save_settings(&AppSettings::new(root.path().to_path_buf()))
                .await
                .is_ok()
        );
        sqlx::query("UPDATE app_settings SET download_directory = 'relative'")
            .execute(&storage.pool)
            .await?;
        assert_eq!(storage.load_settings().await, Err(SettingsError::Storage));
        let path: String = sqlx::query_scalar("SELECT download_directory FROM app_settings")
            .fetch_one(&storage.pool)
            .await?;
        assert_eq!(
            path, "relative",
            "reading corrupt preferences must not overwrite them"
        );
        storage.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn unavailable_saved_folder_is_preserved_and_failed_sql_is_atomic()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join("books");
        tokio::fs::create_dir(&directory).await?;
        let storage = Storage::open(&root.path().join("settings.sqlite3")).await?;
        let Ok(expected) = storage
            .save_settings(&AppSettings::new(directory.clone()))
            .await
        else {
            panic!("save");
        };
        sqlx::query("CREATE TRIGGER reject_settings BEFORE UPDATE ON app_settings BEGIN SELECT RAISE(ABORT, 'test'); END").execute(&storage.pool).await?;
        assert_eq!(
            storage
                .save_settings(&AppSettings::new(root.path().to_path_buf()))
                .await,
            Err(SettingsError::Storage)
        );
        tokio::fs::remove_dir(directory).await?;
        assert_eq!(storage.load_settings().await, Ok(Some(expected)));
        storage.close().await;
        Ok(())
    }
}
