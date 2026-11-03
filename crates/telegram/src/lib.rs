//! Grammers 0.11 transport boundary.
//!
//! Authentication and a secure persistent Session adapter are not implemented yet.
//! The application shell does not create a connection or read credentials.

use std::{error::Error, fmt, sync::Arc};

use grammers_client::{Client, InvocationError, SenderPool};
use grammers_session::Session;
use thiserror::Error;
use tokio::{runtime::Handle, task::JoinHandle};

pub struct ApiCredentials {
    api_id: i32,
    api_hash: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("Telegram API ID must be positive and API hash must contain 32 hexadecimal characters")]
pub struct InvalidCredentials;

impl ApiCredentials {
    pub fn new(api_id: i32, api_hash: String) -> Result<Self, InvalidCredentials> {
        if api_id <= 0 || api_hash.len() != 32 || !api_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidCredentials);
        }
        Ok(Self { api_id, api_hash })
    }

    pub fn api_hash(&self) -> &str {
        &self.api_hash
    }
}

impl fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiCredentials")
            .field("api_id", &"[REDACTED]")
            .field("api_hash", &"[REDACTED]")
            .finish()
    }
}

/// Owns the network task so it cannot outlive its connection accidentally.
/// A session is injected: MemorySession is suitable only for offline tests.
pub struct TelegramConnection {
    client: Client,
    runner: JoinHandle<()>,
}

impl TelegramConnection {
    pub fn start<S>(session: Arc<S>, credentials: &ApiCredentials, runtime: &Handle) -> Self
    where
        S: Session,
        S::Error: Error + Send + Sync + 'static,
    {
        let SenderPool { runner, handle, .. } = SenderPool::new(session, credentials.api_id);
        Self {
            client: Client::new(handle),
            runner: runtime.spawn(runner.run()),
        }
    }

    pub async fn is_authorized(&self) -> Result<bool, InvocationError> {
        self.client.is_authorized().await
    }
}

impl Drop for TelegramConnection {
    fn drop(&mut self) {
        self.client.disconnect();
        self.runner.abort();
    }
}

#[cfg(test)]
mod tests {
    use grammers_session::storages::MemorySession;

    use super::*;

    #[test]
    fn credentials_are_validated_and_debug_is_redacted() -> Result<(), InvalidCredentials> {
        let hash = "0123456789abcdef0123456789abcdef";
        let credentials = ApiCredentials::new(12345, hash.into())?;
        let debug = format!("{credentials:?}");
        assert!(!debug.contains(hash));
        assert!(!debug.contains("12345"));
        assert!(ApiCredentials::new(0, hash.into()).is_err());
        assert!(ApiCredentials::new(1, "invalid".into()).is_err());
        assert!(ApiCredentials::new(1, "z".repeat(32)).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn transport_can_be_constructed_without_contacting_telegram()
    -> Result<(), InvalidCredentials> {
        let credentials = ApiCredentials::new(1, "0".repeat(32))?;
        // The pool connects on demand; do not issue API calls in this test.
        let connection = TelegramConnection::start(
            Arc::new(MemorySession::default()),
            &credentials,
            &Handle::current(),
        );
        drop(connection);
        Ok(())
    }
}
