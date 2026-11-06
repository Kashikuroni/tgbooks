//! Local preferences and persistence port, independent of GPUI and the OS.
use std::{future::Future, path::PathBuf};

use crate::BookFormat;

/// A nonempty subset of the six supported formats. Unknown bits never enter Core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BookFormats(u8);

impl Default for BookFormats {
    fn default() -> Self {
        Self(0b11_1111)
    }
}

impl BookFormats {
    pub fn from_mask(mask: u8) -> Option<Self> {
        (mask != 0 && mask & !0b11_1111 == 0).then_some(Self(mask))
    }
    pub fn mask(self) -> u8 {
        self.0
    }
    pub fn contains(self, format: BookFormat) -> bool {
        self.0 & (1 << Self::index(format)) != 0
    }
    pub fn toggle(self, format: BookFormat) -> Option<Self> {
        Self::from_mask(self.0 ^ (1 << Self::index(format)))
    }
    fn index(format: BookFormat) -> u8 {
        match format {
            BookFormat::Pdf => 0,
            BookFormat::Epub => 1,
            BookFormat::Fb2 => 2,
            BookFormat::Mobi => 3,
            BookFormat::Djvu => 4,
            BookFormat::Zip => 5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppSettings {
    pub download_directory: PathBuf,
    pub formats: BookFormats,
    pub system_notifications: bool,
}

impl AppSettings {
    pub fn new(download_directory: PathBuf) -> Self {
        Self {
            download_directory,
            formats: BookFormats::default(),
            system_notifications: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsError {
    Directory,
    Storage,
    Busy,
}

pub trait SettingsStore: Send + Sync {
    fn load_settings(
        &self,
    ) -> impl Future<Output = Result<Option<AppSettings>, SettingsError>> + Send {
        async { Ok(None) }
    }
    /// Validate the existing destination and atomically save both preferences.
    /// Return the accepted canonical path. Failure must leave prior values intact.
    fn save_settings(
        &self,
        _settings: &AppSettings,
    ) -> impl Future<Output = Result<AppSettings, SettingsError>> + Send {
        async { Err(SettingsError::Storage) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_are_nonempty_known_and_round_trip_without_reordering() {
        assert!(BookFormats::from_mask(0).is_none());
        assert!(BookFormats::from_mask(64).is_none());
        for mask in 1..64 {
            let Some(formats) = BookFormats::from_mask(mask) else {
                panic!("valid mask");
            };
            assert_eq!(formats.mask(), mask);
            for (index, format) in BookFormat::ALL.into_iter().enumerate() {
                assert_eq!(formats.contains(format), mask & (1 << index) != 0);
            }
        }
        let Some(pdf) = BookFormats::from_mask(1) else {
            panic!("pdf");
        };
        assert!(pdf.toggle(BookFormat::Pdf).is_none());
    }
}
