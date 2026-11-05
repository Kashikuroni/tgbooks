use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    Pdf,
    Epub,
    Fb2,
    Mobi,
    Djvu,
    Zip,
}

impl BookFormat {
    pub const ALL: [Self; 6] = [
        Self::Pdf,
        Self::Epub,
        Self::Fb2,
        Self::Mobi,
        Self::Djvu,
        Self::Zip,
    ];

    pub fn from_file_name(name: &str) -> Option<Self> {
        match Path::new(name)
            .extension()?
            .to_str()?
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" => Some(Self::Pdf),
            "epub" => Some(Self::Epub),
            "fb2" => Some(Self::Fb2),
            "mobi" => Some(Self::Mobi),
            "djvu" => Some(Self::Djvu),
            "zip" => Some(Self::Zip),
            _ => None,
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Epub => "epub",
            Self::Fb2 => "fb2",
            Self::Mobi => "mobi",
            Self::Djvu => "djvu",
            Self::Zip => "zip",
        }
    }
}

/// Message identifiers are scoped to a channel; document IDs are not filenames.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DownloadKey {
    /// Bot API dialog ID for records discovered from Telegram channels.
    pub channel_id: i64,
    pub message_id: i32,
    pub document_id: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Completed,
    Failed,
}

impl DownloadStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Unknown length is distinct from a known empty file.
/// Private fields keep invalid progress out of the UI and database adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DownloadProgress {
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

#[derive(Debug, Error)]
#[error("downloaded bytes exceed the total file size")]
pub struct InvalidProgress;

impl DownloadProgress {
    pub fn new(downloaded_bytes: u64, total_bytes: Option<u64>) -> Result<Self, InvalidProgress> {
        if total_bytes.is_some_and(|total| downloaded_bytes > total) {
            return Err(InvalidProgress);
        }
        Ok(Self {
            downloaded_bytes,
            total_bytes,
        })
    }

    pub const fn downloaded_bytes(self) -> u64 {
        self.downloaded_bytes
    }

    pub const fn total_bytes(self) -> Option<u64> {
        self.total_bytes
    }

    pub fn percentage(self) -> Option<u8> {
        self.total_bytes.map(|total| {
            if total == 0 {
                100
            } else {
                // Widen before multiplying so very large files cannot overflow.
                ((u128::from(self.downloaded_bytes) * 100) / u128::from(total)) as u8
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DownloadRecord {
    pub key: DownloadKey,
    pub file_name: String,
    pub local_path: String,
    pub status: DownloadStatus,
    pub progress: DownloadProgress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_formats_are_case_insensitive_and_use_the_last_extension() {
        for format in BookFormat::ALL {
            let name = format!("Книга.{}", format.extension().to_uppercase());
            assert_eq!(BookFormat::from_file_name(&name), Some(format));
        }
        for name in [
            "",
            "pdf",
            ".pdf",
            "book.pdf.exe",
            "book.txt",
            "book.fb2.zip.exe",
        ] {
            assert_eq!(BookFormat::from_file_name(name), None);
        }
    }

    #[test]
    fn progress_handles_unknown_empty_invalid_and_large_files() -> Result<(), InvalidProgress> {
        assert_eq!(DownloadProgress::new(5, None)?.percentage(), None);
        assert_eq!(DownloadProgress::new(0, Some(0))?.percentage(), Some(100));
        assert_eq!(DownloadProgress::new(1, Some(3))?.percentage(), Some(33));
        assert_eq!(
            DownloadProgress::new(u64::MAX, Some(u64::MAX))?.percentage(),
            Some(100)
        );
        assert!(DownloadProgress::new(2, Some(1)).is_err());
        Ok(())
    }

    #[test]
    fn formats_have_stable_serialized_names() -> Result<(), serde_json::Error> {
        assert_eq!(serde_json::to_string(&BookFormat::Epub)?, "\"epub\"");
        assert_eq!(
            serde_json::from_str::<BookFormat>("\"epub\"")?,
            BookFormat::Epub
        );
        Ok(())
    }
}
