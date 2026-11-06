//! Read-only channel discovery. Access hashes and Grammers objects stay here.

use std::{collections::HashMap, future::Future, panic::AssertUnwindSafe, time::Duration};

use futures_util::FutureExt as _;
use grammers_client::{
    InvocationError,
    media::{Document, Media},
    message::Message,
    peer::Peer,
    tl,
};
use tgbooks_core::{
    BookAttachment, BookPage, CatalogError, ChannelCatalog, ChannelSummary, DiscoveredBook,
    DownloadKey, MonitorPage, SCAN_PAGE_SIZE,
};

use crate::TelegramAuth;

impl ChannelCatalog for TelegramAuth {
    async fn list_channels(&mut self) -> Result<Vec<ChannelSummary>, CatalogError> {
        let client = self.catalog_client()?;
        let (channels, references) = guarded(
            async move {
                let mut dialogs = client.iter_dialogs();
                let mut channels = Vec::new();
                let mut references = HashMap::new();
                while let Some(dialog) = dialogs.next().await.map_err(map_error)? {
                    let Peer::Channel(channel) = dialog.peer() else {
                        continue;
                    };
                    if channel.raw.left {
                        continue;
                    }
                    let id = channel
                        .id()
                        .bot_api_dialog_id()
                        .ok_or(CatalogError::InvalidResponse)?;
                    references.insert(id, dialog.peer_ref());
                    channels.push(ChannelSummary {
                        id,
                        title: if channel.title().trim().is_empty() {
                            format!("Канал {id}")
                        } else {
                            channel.title().to_owned()
                        },
                        username: channel
                            .username()
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned),
                        // This friendly type exposes no accessor for the flag in 0.11.
                        protected: channel.raw.noforwards,
                    });
                }
                Ok((channels, references))
            },
            Duration::from_secs(60),
        )
        .await?;
        // Do not replace usable references with a partially fetched list.
        self.channels = references;
        Ok(channels)
    }

    async fn scan_books(
        &mut self,
        channel_id: i64,
        before: Option<i32>,
    ) -> Result<BookPage, CatalogError> {
        let page = self.read_updates(channel_id, before).await?;
        Ok(BookPage {
            books: page.books.into_iter().map(|found| found.book).collect(),
            scanned: page.scanned,
            skipped_protected: page.skipped_protected,
            next_offset: page.next_offset,
        })
    }

    async fn scan_updates(
        &mut self,
        channel_id: i64,
        before: Option<i32>,
    ) -> Result<MonitorPage, CatalogError> {
        self.read_updates(channel_id, before).await
    }
}

impl TelegramAuth {
    async fn read_updates(
        &mut self,
        channel_id: i64,
        before: Option<i32>,
    ) -> Result<MonitorPage, CatalogError> {
        let client = self.catalog_client()?;
        let reference = self
            .channels
            .get(&channel_id)
            .cloned()
            .ok_or(CatalogError::Unavailable)?;
        guarded(
            async move {
                // A cached dialog is a snapshot. Re-check membership and protection
                // immediately before reading history, rather than trusting old flags.
                let peer = client.resolve_peer(reference).await.map_err(map_error)?;
                let Peer::Channel(channel) = peer else {
                    return Err(CatalogError::Unavailable);
                };
                if channel.raw.left {
                    return Err(CatalogError::Unavailable);
                }
                if channel.raw.noforwards {
                    return Err(CatalogError::ProtectedContent);
                }
                let mut messages = client
                    .iter_messages(reference)
                    .offset_id(before.unwrap_or(0))
                    .limit(SCAN_PAGE_SIZE);
                let mut books = Vec::new();
                let mut scanned = 0;
                let mut skipped_protected = 0;
                let mut oldest: Option<i32> = None;
                let mut newest: Option<i32> = None;
                while let Some(message) = messages.next().await.map_err(map_error)? {
                    scanned += 1;
                    let id = message.id();
                    if id > 0 {
                        oldest = Some(oldest.map_or(id, |old| old.min(id)));
                        newest = Some(newest.map_or(id, |new| new.max(id)));
                    }
                    if is_protected(&message) {
                        skipped_protected += 1;
                        continue;
                    }
                    let Some(document) = book_document(&message) else {
                        continue;
                    };
                    let Some(name) = document.name() else {
                        continue;
                    };
                    if let Some(book) = BookAttachment::discover(
                        DownloadKey {
                            channel_id,
                            message_id: id,
                            document_id: document.id(),
                        },
                        name.to_owned(),
                        document.size().and_then(|size| u64::try_from(size).ok()),
                        false,
                    ) {
                        books.push(DiscoveredBook {
                            book,
                            message_date: message.date().as_second(),
                        });
                    }
                }
                let next_offset = if scanned as usize == SCAN_PAGE_SIZE {
                    Some(oldest.ok_or(CatalogError::InvalidResponse)?)
                } else {
                    None
                };
                Ok(MonitorPage {
                    books,
                    scanned,
                    skipped_protected,
                    newest,
                    next_offset,
                })
            },
            Duration::from_secs(30),
        )
        .await
    }
}

pub(crate) fn is_protected(message: &Message) -> bool {
    matches!(&message.raw, tl::enums::Message::Message(raw) if raw.noforwards)
        || matches!(message.peer(), Some(Peer::Channel(peer)) if peer.raw.noforwards)
        || message
            .restriction_reason()
            .is_some_and(|reasons| !reasons.is_empty())
        || matches!(message.media(), Some(Media::Document(document)) if document.raw.ttl_seconds.is_some())
}

pub(crate) fn book_document(message: &Message) -> Option<Document> {
    let Some(Media::Document(document)) = message.media() else {
        return None;
    };
    // The SDK's name()/id() require an actual, non-empty document.
    matches!(
        &document.raw.document,
        Some(tl::enums::Document::Document(_))
    )
    .then_some(document)
}

pub(crate) async fn guarded<T>(
    operation: impl Future<Output = Result<T, CatalogError>>,
    timeout: Duration,
) -> Result<T, CatalogError> {
    match tokio::time::timeout(timeout, AssertUnwindSafe(operation).catch_unwind()).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(CatalogError::UnsupportedReply),
        Err(_) => Err(CatalogError::Network),
    }
}

pub(crate) fn map_error(error: InvocationError) -> CatalogError {
    match error {
        InvocationError::Session(_) => CatalogError::Storage,
        InvocationError::Rpc(error) if error.code == 401 => CatalogError::Unauthorized,
        InvocationError::Rpc(error) if error.code == 420 => {
            CatalogError::FloodWait(error.value.unwrap_or(60).max(1))
        }
        InvocationError::Rpc(_) => CatalogError::Unavailable,
        _ => CatalogError::Network,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discovery_before_login_does_not_open_sessions_or_connect()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut backend = TelegramAuth::new(directory.path().to_owned());
        assert_eq!(
            backend.list_channels().await,
            Err(CatalogError::Unauthorized)
        );
        assert_eq!(
            backend.scan_books(-1000000000001, None).await,
            Err(CatalogError::Unauthorized)
        );
        assert_eq!(
            backend.scan_updates(-1000000000001, None).await,
            Err(CatalogError::Unauthorized)
        );
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn discovery_errors_do_not_expose_rpc_payloads() {
        let error = InvocationError::Rpc(grammers_client::sender::RpcError {
            code: 420,
            name: "FLOOD_WAIT".into(),
            value: Some(60),
            caused_by: None,
        });
        assert_eq!(map_error(error), CatalogError::FloodWait(60));
        let error = InvocationError::Rpc(grammers_client::sender::RpcError {
            code: 401,
            name: "AUTH_KEY_UNREGISTERED".into(),
            value: None,
            caused_by: None,
        });
        assert_eq!(map_error(error), CatalogError::Unauthorized);
    }
}
