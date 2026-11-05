//! Composition root for local adapters. GPUI views never import this module.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use tgbooks_core::{AppInfo, CoreEvent, ServiceConnection, ServiceFailure, run_service};
use tgbooks_downloader::LocalDownloader;
use tgbooks_storage::{AppLock, AppLockError, AppPaths, Storage};
use tgbooks_telegram::TelegramAuth;

pub async fn run(connection: ServiceConnection) {
    let initialized = async {
        let paths = match std::env::var_os("TGBOOKS_DATA_DIR") {
            Some(path) => AppPaths::in_directory(PathBuf::from(path))?,
            None => AppPaths::discover()?,
        };
        let directory = paths.data_directory().to_owned();
        paths.prepare().await?;
        let lock = AppLock::acquire(&directory).await?;
        let (storage, info) = initialize(paths).await?;
        Ok::<_, anyhow::Error>((storage, info, TelegramAuth::new(directory), lock))
    }
    .await;

    match initialized {
        Ok((storage, info, authentication, _lock)) => {
            tracing::info!("local backend ready");
            run_service(
                &storage,
                authentication,
                LocalDownloader::new(storage.clone()),
                info,
                connection,
            )
            .await;
            storage.close().await;
            tracing::info!("local backend stopped");
        }
        Err(error) => {
            tracing::error!(error = %error, "local backend initialization failed");
            let failure = if matches!(
                error.downcast_ref::<AppLockError>(),
                Some(AppLockError::AlreadyRunning)
            ) {
                ServiceFailure::AlreadyRunning
            } else {
                ServiceFailure::Initialization
            };
            let _ = connection.events.send(CoreEvent::Failed(failure)).await;
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
#[path = "backend_download_tests.rs"]
mod download_tests;

#[cfg(test)]
mod tests {
    use tgbooks_core::{
        CoreCommand, DownloadKey, DownloadProgress, DownloadRecord, DownloadStatus,
    };

    use super::*;

    struct RestoredAuth;

    impl tgbooks_core::BookSource for RestoredAuth {}

    impl tgbooks_core::Authentication for RestoredAuth {
        async fn execute(
            &mut self,
            _: tgbooks_core::AuthAction,
        ) -> Result<tgbooks_core::AuthStep, tgbooks_core::AuthFailure> {
            Ok(tgbooks_core::AuthStep::Authorized)
        }
    }

    impl tgbooks_core::ChannelCatalog for RestoredAuth {
        async fn list_channels(
            &mut self,
        ) -> Result<Vec<tgbooks_core::ChannelSummary>, tgbooks_core::CatalogError> {
            Ok(Vec::new())
        }
        async fn scan_books(
            &mut self,
            _: i64,
            _: Option<i32>,
        ) -> Result<tgbooks_core::BookPage, tgbooks_core::CatalogError> {
            Err(tgbooks_core::CatalogError::Unavailable)
        }
    }

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
            run_service(
                &storage,
                RestoredAuth,
                LocalDownloader::new(storage.clone()),
                info,
                backend,
            )
            .await;
            storage.close().await;
        });
        assert!(matches!(ui.events.recv().await?, CoreEvent::Ready(_)));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(vec![record.clone()])
        );
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::Authentication(tgbooks_core::AuthState { busy: true, .. })
        ));
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::Authentication(tgbooks_core::AuthState {
                step: tgbooks_core::AuthStep::Authorized,
                busy: false,
                ..
            })
        ));
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::Catalog(tgbooks_core::CatalogState {
                operation: Some(tgbooks_core::CatalogOperation::ListChannels),
                ..
            })
        ));
        assert!(matches!(
            ui.events.recv().await?,
            CoreEvent::Catalog(tgbooks_core::CatalogState {
                authenticated: true,
                operation: None,
                ..
            })
        ));
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
