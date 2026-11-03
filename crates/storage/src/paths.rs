use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("the system application data directory is unavailable")]
    Unavailable,
    #[error("the application data directory must be an absolute path")]
    RelativePath,
    #[error("could not create the application data directory")]
    Create(#[from] std::io::Error),
}

/// All platform-specific directory resolution lives in this adapter.
#[derive(Clone, Debug)]
pub struct AppPaths {
    data: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, PathsError> {
        let project = ProjectDirs::from("org", "tgbooks", "TelegramBookDownloader")
            .ok_or(PathsError::Unavailable)?;
        Self::in_directory(project.data_local_dir().to_path_buf())
    }

    /// Explicit override for local development and isolated smoke tests.
    pub fn in_directory(path: PathBuf) -> Result<Self, PathsError> {
        if !path.is_absolute() {
            return Err(PathsError::RelativePath);
        }
        Ok(Self { data: path })
    }

    pub fn data_directory(&self) -> &Path {
        &self.data
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("downloads.sqlite3")
    }

    pub fn download_directory(&self) -> PathBuf {
        self.data.join("books")
    }

    pub async fn prepare(&self) -> Result<(), PathsError> {
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&self.data).await?;
        builder.create(self.download_directory()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_rejected() {
        assert!(matches!(
            AppPaths::in_directory(PathBuf::from("data")),
            Err(PathsError::RelativePath)
        ));
    }

    #[tokio::test]
    async fn data_and_downloads_are_created_inside_the_selected_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let paths = AppPaths::in_directory(temporary.path().join("app"))?;
        paths.prepare().await?;
        paths.prepare().await?;
        assert!(paths.data_directory().is_dir());
        assert!(paths.download_directory().is_dir());
        assert!(!paths.database().exists());
        Ok(())
    }
}
