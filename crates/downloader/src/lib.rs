//! Download admission and FIFO ordering, independent of Telegram and the UI.
//! File transfer and persistent queue recovery belong to subsequent iterations.

use std::collections::{HashSet, VecDeque};

use tgbooks_core::{BookFormat, DownloadKey};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadCandidate {
    pub key: DownloadKey,
    pub file_name: String,
    pub protected: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AdmissionError {
    #[error("protected content cannot be downloaded")]
    ProtectedContent,
    #[error("unsupported book format")]
    UnsupportedFormat,
}

#[derive(Default)]
pub struct DownloadQueue {
    pending: VecDeque<DownloadCandidate>,
    // Retain admitted keys after pop: an in-flight job must not be admitted again.
    // Persistent, cross-restart deduplication is the repository's responsibility.
    admitted: HashSet<DownloadKey>,
}

impl DownloadQueue {
    /// Returns false for a duplicate. No Telegram request is needed for admission.
    pub fn enqueue(&mut self, candidate: DownloadCandidate) -> Result<bool, AdmissionError> {
        if candidate.protected {
            return Err(AdmissionError::ProtectedContent);
        }
        if BookFormat::from_file_name(&candidate.file_name).is_none() {
            return Err(AdmissionError::UnsupportedFormat);
        }
        if !self.admitted.insert(candidate.key) {
            return Ok(false);
        }
        self.pending.push_back(candidate);
        Ok(true)
    }

    pub fn pop(&mut self) -> Option<DownloadCandidate> {
        self.pending.pop_front()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(channel_id: i64, message_id: i32) -> DownloadCandidate {
        DownloadCandidate {
            key: DownloadKey {
                channel_id,
                message_id,
                document_id: 99,
            },
            file_name: "Книга.EPUB".into(),
            protected: false,
        }
    }

    #[test]
    fn queue_is_fifo_and_deduplicates_pending_and_in_flight_jobs() -> Result<(), AdmissionError> {
        let mut queue = DownloadQueue::default();
        let first = candidate(1, 1);
        let second = candidate(1, 2);
        assert!(queue.enqueue(first.clone())?);
        assert!(!queue.enqueue(first.clone())?);
        assert!(queue.enqueue(second.clone())?);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(first.clone()));
        assert!(!queue.enqueue(first)?);
        assert_eq!(queue.pop(), Some(second));
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn identical_message_ids_in_different_channels_are_not_duplicates() -> Result<(), AdmissionError>
    {
        let mut queue = DownloadQueue::default();
        assert!(queue.enqueue(candidate(1, 1))?);
        assert!(queue.enqueue(candidate(2, 1))?);
        Ok(())
    }

    #[test]
    fn protected_and_unsupported_files_never_enter_the_queue() {
        let mut queue = DownloadQueue::default();
        let mut protected = candidate(1, 1);
        protected.protected = true;
        assert_eq!(
            queue.enqueue(protected),
            Err(AdmissionError::ProtectedContent)
        );
        let mut unsupported = candidate(1, 2);
        unsupported.file_name = "book.exe".into();
        assert_eq!(
            queue.enqueue(unsupported),
            Err(AdmissionError::UnsupportedFormat)
        );
        assert!(queue.is_empty());
    }
}
