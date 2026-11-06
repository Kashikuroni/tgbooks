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
                    let chunks = Box::new(TelegramChunks {
                        iter: Some(client.iter_download(&document)),
                        skip_prefix: 0,
                    });
                    Ok(RemoteBook {
                        book,
                        source_title: Some(channel.title().to_owned()),
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

struct TelegramChunks {
    iter: Option<DownloadIter>,
    skip_prefix: usize,
}

// Official Grammers DownloadIter defaults to 512 KiB. skip_chunks changes the
// request offset in those units; discard the leading bytes for an exact seek.
const CHUNK_SIZE: u64 = 512 * 1024;

fn resume_position(offset: u64) -> Result<(i32, usize), DownloadError> {
    Ok((
        i32::try_from(offset / CHUNK_SIZE).map_err(|_| DownloadError::InvalidFile)?,
        (offset % CHUNK_SIZE) as usize,
    ))
}

impl BookChunks for TelegramChunks {
    fn seek(&mut self, offset: u64) -> Result<(), DownloadError> {
        let (chunks, prefix) = resume_position(offset)?;
        let iter = self.iter.take().ok_or(DownloadError::Internal)?;
        self.iter = Some(iter.skip_chunks(chunks));
        self.skip_prefix = prefix;
        Ok(())
    }

    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async {
            let chunk = guarded(
                async {
                    self.iter
                        .as_mut()
                        .ok_or(CatalogError::InvalidResponse)?
                        .next()
                        .await
                        .map_err(map_error)
                },
                Duration::from_secs(30),
            )
            .await
            .map_err(download_error)?;
            match chunk {
                Some(mut bytes) => {
                    if self.skip_prefix > bytes.len() {
                        return Err(DownloadError::Incomplete);
                    }
                    bytes.drain(..self.skip_prefix);
                    self.skip_prefix = 0;
                    if bytes.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(bytes))
                    }
                }
                None if self.skip_prefix != 0 => Err(DownloadError::Incomplete),
                None => Ok(None),
            }
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

    #[test]
    fn resume_offsets_are_aligned_without_losing_the_exact_byte_position()
    -> Result<(), DownloadError> {
        for offset in [
            0,
            3,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 123,
            1_400_000_000,
        ] {
            let (chunks, prefix) = resume_position(offset)?;
            assert_eq!(chunks as u64 * CHUNK_SIZE + prefix as u64, offset);
            assert!(prefix < CHUNK_SIZE as usize);
        }
        assert_eq!(resume_position(u64::MAX), Err(DownloadError::InvalidFile));
        Ok(())
    }

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
