//! One application owns a data directory before any recovery writes are made.

use std::{
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

use thiserror::Error;

pub struct AppLock {
    _file: File,
}

#[derive(Debug, Error)]
pub enum AppLockError {
    #[error("another application instance is using this data directory")]
    AlreadyRunning,
    #[error("could not lock the application data directory")]
    Io,
}

impl AppLock {
    pub async fn acquire(directory: &Path) -> Result<Self, AppLockError> {
        let path = directory.join("tgbooks.app.lock");
        tokio::task::spawn_blocking(move || {
            let mut options = OpenOptions::new();
            options.create(true).truncate(false).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let file = options.open(path).map_err(|_| AppLockError::Io)?;
            file.try_lock().map_err(|error| match error {
                TryLockError::WouldBlock => AppLockError::AlreadyRunning,
                TryLockError::Error(_) => AppLockError::Io,
            })?;
            Ok(Self { _file: file })
        })
        .await
        .map_err(|_| AppLockError::Io)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn directory_lock_is_exclusive_and_released_without_deleting_its_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let first = AppLock::acquire(temporary.path()).await?;
        assert!(matches!(
            AppLock::acquire(temporary.path()).await,
            Err(AppLockError::AlreadyRunning)
        ));
        drop(first);
        let _next = AppLock::acquire(temporary.path()).await?;
        Ok(())
    }
}
