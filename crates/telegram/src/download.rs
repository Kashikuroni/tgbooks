//! Read-only preparation of an owned, streaming download. No filesystem access.

use std::time::Duration;

use grammers_client::{client::DownloadIter, peer::Peer};
use tgbooks_core::{
    BookAttachment, BookChunks, BookFuture, BookSource, CatalogError, ChunkFuture, DownloadError,
    DownloadKey, RemoteBook,
};

use crate::{
    TelegramAuth,
    channels::{book_document, guarded, is_protected, map_error},
};

impl BookSource for TelegramAuth {
    fn open_book(&self, key: DownloadKey) -> BookFuture {
        let client = self.catalog_client();
        let reference = self.channels.get(&key.channel_id).cloned();
        Box::pin(async move {
            guarded(
                async move {
                    let client = client?;
                    let reference = reference.ok_or(CatalogError::Unavailable)?;
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
                    let message = client
                        .get_messages_by_id(reference, &[key.message_id])
                        .await
                        .map_err(map_error)?
                        .into_iter()
                        .flatten()
                        .next()
                        .ok_or(CatalogError::Unavailable)?;
                    if is_protected(&message) {
                        return Err(CatalogError::ProtectedContent);
                    }
                    let document = book_document(&message).ok_or(CatalogError::Unavailable)?;
                    // An edited message must not silently download another document.
                    if document.id() != key.document_id {
                        return Err(CatalogError::Unavailable);
                    }
                    let name = document.name().ok_or(CatalogError::Unavailable)?;
                    let book = BookAttachment::discover(
                        key,
                        name.to_owned(),
                        document.size().and_then(|size| u64::try_from(size).ok()),
                        false,
                    )
                    .ok_or(CatalogError::Unavailable)?;
                    let chunks = Box::new(TelegramChunks(client.iter_download(&document)));
                    Ok(RemoteBook {
                        book,
                        chunks: chunks as Box<dyn BookChunks>,
                    })
                },
                Duration::from_secs(30),
            )
            .await
            .map_err(download_error)
        })
    }
}

struct TelegramChunks(DownloadIter);

impl BookChunks for TelegramChunks {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async {
            guarded(
                async { self.0.next().await.map_err(map_error) },
                Duration::from_secs(30),
            )
            .await
            .map_err(download_error)
        })
    }
}

fn download_error(error: CatalogError) -> DownloadError {
    match error {
        CatalogError::Busy => DownloadError::Busy,
        CatalogError::Unauthorized => DownloadError::Unauthorized,
        CatalogError::Unavailable => DownloadError::Unavailable,
        CatalogError::ProtectedContent => DownloadError::ProtectedContent,
        CatalogError::Network => DownloadError::Network,
        CatalogError::Storage => DownloadError::SecureStorage,
        CatalogError::FloodWait(seconds) => DownloadError::FloodWait(seconds),
        CatalogError::InvalidResponse | CatalogError::UnsupportedReply => {
            DownloadError::UnsupportedReply
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn download_before_login_does_not_open_a_session_or_contact_telegram()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let source = TelegramAuth::new(directory.path().to_owned());
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 1,
            document_id: 2,
        };
        assert!(matches!(
            source.open_book(key).await,
            Err(DownloadError::Unauthorized)
        ));
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }
}
