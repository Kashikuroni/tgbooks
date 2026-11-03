//! Composition root for local adapters. GPUI views never import this module.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use tgbooks_core::{AppInfo, CoreEvent, ServiceConnection, ServiceFailure, run_service};
use tgbooks_storage::{AppPaths, Storage};

pub async fn run(connection: ServiceConnection) {
    let initialized = async {
        let paths = match std::env::var_os("TGBOOKS_DATA_DIR") {
            Some(path) => AppPaths::in_directory(PathBuf::from(path))?,
            None => AppPaths::discover()?,
        };
        initialize(paths).await
    }
    .await;

    match initialized {
        Ok((storage, info)) => {
            tracing::info!("local backend ready");
            run_service(&storage, info, connection).await;
            storage.close().await;
            tracing::info!("local backend stopped");
        }
        Err(error) => {
            tracing::error!(error = %error, "local backend initialization failed");
            let _ = connection
                .events
                .send(CoreEvent::Failed(ServiceFailure::Initialization))
                .await;
        }
    }
}

async fn initialize(paths: AppPaths) -> Result<(Storage, AppInfo)> {
    paths
        .prepare()
        .await
        .context("could not prepare application directories")?;
    let storage = Storage::open(&paths.database())
        .await
        .context("could not open download history")?;
    let info = AppInfo {
        data_directory: paths.data_directory().to_string_lossy().into_owned(),
        download_directory: paths.download_directory().to_string_lossy().into_owned(),
    };
    Ok((storage, info))
}

#[cfg(test)]
mod tests {
    use tgbooks_core::{
        CoreCommand, DownloadKey, DownloadProgress, DownloadRecord, DownloadStatus,
    };

    use super::*;

    #[tokio::test]
    async fn sqlite_history_reaches_the_ui_protocol_after_restart() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let paths = AppPaths::in_directory(temporary.path().join("application"))?;
        let (storage, _) = initialize(paths.clone()).await?;
        let record = DownloadRecord {
            key: DownloadKey {
                channel_id: 1,
                message_id: 2,
                document_id: 3,
            },
            file_name: "book.epub".into(),
            local_path: paths
                .download_directory()
                .join("book.epub")
                .to_string_lossy()
                .into_owned(),
            status: DownloadStatus::Completed,
            progress: DownloadProgress::new(512, Some(512))?,
        };
        storage.insert_download(&record).await?;
        storage.close().await;

        let (storage, info) = initialize(paths).await?;
        let (ui, backend) = tgbooks_core::connect();
        let task = tokio::spawn(async move {
            run_service(&storage, info, backend).await;
            storage.close().await;
        });
        assert!(matches!(ui.events.recv().await?, CoreEvent::Ready(_)));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(vec![record.clone()])
        );
        ui.commands.send(CoreCommand::RefreshHistory).await?;
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(vec![record])
        );
        ui.commands.send(CoreCommand::Shutdown).await?;
        task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_database_location_returns_an_error_instead_of_panicking() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let file = temporary.path().join("not-a-directory");
        tokio::fs::write(&file, b"occupied").await?;
        let paths = AppPaths::in_directory(file)?;
        assert!(initialize(paths).await.is_err());
        Ok(())
    }
}
