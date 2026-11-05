//! Channel selection and paged book discovery, independent of Telegram and GPUI.

use std::{collections::HashSet, future::Future};

use crate::{BookFormat, DownloadKey};

pub const SCAN_PAGE_SIZE: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelSummary {
    /// Telegram Bot API dialog ID, not a title/username or MTProto access hash.
    pub id: i64,
    pub title: String,
    pub username: Option<String>,
    pub protected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookAttachment {
    pub key: DownloadKey,
    pub file_name: String,
    pub size_bytes: Option<u64>,
    pub format: BookFormat,
}

impl BookAttachment {
    pub fn discover(
        key: DownloadKey,
        file_name: String,
        size_bytes: Option<u64>,
        protected: bool,
    ) -> Option<Self> {
        if protected {
            return None;
        }
        let format = BookFormat::from_file_name(&file_name)?;
        Some(Self {
            key,
            file_name,
            size_bytes,
            format,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookPage {
    pub books: Vec<BookAttachment>,
    pub scanned: u32,
    pub skipped_protected: u32,
    /// Exclusive message offset for the next page. None means history is exhausted.
    pub next_offset: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogError {
    Busy,
    Unauthorized,
    Unavailable,
    ProtectedContent,
    Network,
    Storage,
    FloodWait(u32),
    InvalidResponse,
    UnsupportedReply,
}

pub trait ChannelCatalog: Send {
    fn list_channels(
        &mut self,
    ) -> impl Future<Output = Result<Vec<ChannelSummary>, CatalogError>> + Send;

    fn scan_books(
        &mut self,
        channel_id: i64,
        before: Option<i32>,
    ) -> impl Future<Output = Result<BookPage, CatalogError>> + Send;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogOperation {
    ListChannels,
    ScanHistory,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogState {
    pub authenticated: bool,
    pub channels: Vec<ChannelSummary>,
    pub selected: Option<i64>,
    pub books: Vec<BookAttachment>,
    pub scanned_messages: u64,
    pub skipped_protected: u64,
    pub scan_started: bool,
    pub next_offset: Option<i32>,
    pub operation: Option<CatalogOperation>,
    pub error: Option<CatalogError>,
}

impl CatalogState {
    pub fn selected_channel(&self) -> Option<&ChannelSummary> {
        self.selected
            .and_then(|id| self.channels.iter().find(|channel| channel.id == id))
    }

    pub fn busy(&self) -> bool {
        self.operation.is_some()
    }

    pub fn can_scan(&self) -> bool {
        self.authenticated
            && !self.busy()
            && self
                .selected_channel()
                .is_some_and(|channel| !channel.protected)
            && (!self.scan_started || self.next_offset.is_some())
    }

    pub(crate) fn select(&mut self, id: Option<i64>) -> Result<(), CatalogError> {
        if id.is_some_and(|id| !self.channels.iter().any(|channel| channel.id == id)) {
            return Err(CatalogError::Unavailable);
        }
        if self.selected != id {
            self.selected = id;
            self.reset_scan();
        }
        self.error = None;
        Ok(())
    }

    fn reset_scan(&mut self) {
        self.books.clear();
        self.scanned_messages = 0;
        self.skipped_protected = 0;
        self.scan_started = false;
        self.next_offset = None;
    }

    pub(crate) fn replace_channels(&mut self, mut channels: Vec<ChannelSummary>) {
        let mut ids = HashSet::new();
        channels.retain(|channel| ids.insert(channel.id));
        channels.sort_by_cached_key(|channel| (channel.title.to_lowercase(), channel.id));
        self.channels = channels;
        if self.selected.is_some() && self.selected_channel().is_none() {
            self.selected = None;
            self.reset_scan();
        } else if self
            .selected_channel()
            .is_some_and(|channel| channel.protected)
        {
            self.reset_scan();
        }
    }

    pub(crate) fn append_page(&mut self, page: BookPage) -> Result<(), CatalogError> {
        let channel_id = self.selected.ok_or(CatalogError::Unavailable)?;
        if page.scanned as usize > SCAN_PAGE_SIZE
            || page.skipped_protected > page.scanned
            || page.books.len() > page.scanned as usize
            || page.books.iter().any(|book| {
                book.key.channel_id != channel_id
                    || book.key.message_id <= 0
                    || BookFormat::from_file_name(&book.file_name) != Some(book.format)
            })
            || page.next_offset.is_some_and(|next| {
                next <= 0
                    || page.scanned == 0
                    || self.next_offset.is_some_and(|before| next >= before)
            })
        {
            return Err(CatalogError::InvalidResponse);
        }
        let mut known: HashSet<_> = self.books.iter().map(|book| book.key).collect();
        self.books
            .extend(page.books.into_iter().filter(|book| known.insert(book.key)));
        self.scanned_messages = self
            .scanned_messages
            .saturating_add(u64::from(page.scanned));
        self.skipped_protected = self
            .skipped_protected
            .saturating_add(u64::from(page.skipped_protected));
        self.scan_started = true;
        self.next_offset = page.next_offset;
        Ok(())
    }

    pub(crate) fn reject(&mut self, error: CatalogError) {
        self.operation = None;
        self.error = Some(error);
        if error == CatalogError::ProtectedContent {
            if let Some(channel) = self
                .channels
                .iter_mut()
                .find(|channel| Some(channel.id) == self.selected)
            {
                channel.protected = true;
            }
            self.reset_scan();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: i64, protected: bool) -> ChannelSummary {
        ChannelSummary {
            id,
            title: format!("Channel {id}"),
            username: None,
            protected,
        }
    }

    fn book(message_id: i32) -> BookAttachment {
        BookAttachment {
            key: DownloadKey {
                channel_id: -1000000000001,
                message_id,
                document_id: 99,
            },
            file_name: "Книга.EPUB".into(),
            size_bytes: Some(123),
            format: BookFormat::Epub,
        }
    }

    fn state() -> CatalogState {
        CatalogState {
            authenticated: true,
            channels: vec![channel(-1000000000001, false)],
            selected: Some(-1000000000001),
            ..Default::default()
        }
    }

    #[test]
    fn discovery_uses_supported_extensions_and_never_admits_protected_documents() {
        for format in BookFormat::ALL {
            assert!(
                BookAttachment::discover(
                    book(1).key,
                    format!("Book.{}", format.extension().to_uppercase()),
                    None,
                    false
                )
                .is_some()
            );
        }
        assert!(BookAttachment::discover(book(1).key, "book.pdf".into(), None, true).is_none());
        assert!(
            BookAttachment::discover(book(1).key, "book.pdf.exe".into(), None, false).is_none()
        );
    }

    #[test]
    fn pages_deduplicate_by_telegram_key_and_advance_only_after_success() -> Result<(), CatalogError>
    {
        let mut state = state();
        state.append_page(BookPage {
            books: vec![book(100)],
            scanned: 100,
            skipped_protected: 2,
            next_offset: Some(90),
        })?;
        state.append_page(BookPage {
            books: vec![book(100), book(80)],
            scanned: 20,
            skipped_protected: 0,
            next_offset: None,
        })?;
        assert_eq!(state.books.len(), 2);
        assert_eq!(state.scanned_messages, 120);
        assert_eq!(state.skipped_protected, 2);
        assert!(!state.can_scan());
        Ok(())
    }

    #[test]
    fn malformed_or_non_advancing_pages_do_not_modify_results() -> Result<(), CatalogError> {
        let mut state = state();
        state.append_page(BookPage {
            books: vec![book(100)],
            scanned: 100,
            skipped_protected: 0,
            next_offset: Some(90),
        })?;
        let before = state.clone();
        assert_eq!(
            state.append_page(BookPage {
                books: vec![book(80)],
                scanned: 100,
                skipped_protected: 0,
                next_offset: Some(90)
            }),
            Err(CatalogError::InvalidResponse)
        );
        assert_eq!(state, before);
        Ok(())
    }

    #[test]
    fn selection_and_access_changes_clear_stale_books() -> Result<(), CatalogError> {
        let mut state = state();
        state.append_page(BookPage {
            books: vec![book(1)],
            scanned: 1,
            skipped_protected: 0,
            next_offset: None,
        })?;
        state.replace_channels(vec![channel(-1000000000001, true)]);
        assert!(state.books.is_empty());
        assert!(!state.can_scan());
        state.replace_channels(Vec::new());
        assert!(state.selected.is_none());
        assert_eq!(state.select(Some(999)), Err(CatalogError::Unavailable));
        Ok(())
    }
}
