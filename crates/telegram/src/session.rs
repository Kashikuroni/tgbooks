//! Write-through Grammers Session implementation. Async mutations are committed
//! on a blocking worker before they become visible to the MTProto client.

use std::sync::{Arc, Mutex};

use grammers_session::{
    BoxFuture, Session, SessionData,
    types::{ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState},
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{
    ApiCredentials,
    vault::{SecretKey, Vault, VaultError},
};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Snapshot {
    api_id: i32,
    api_hash: String,
    home_dc: i32,
    dc_options: Vec<DcOption>,
    peers: Vec<PeerInfo>,
    updates: UpdatesState,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.api_hash.zeroize();
        for dc in &mut self.dc_options {
            if let Some(key) = dc.auth_key.as_mut() {
                key.zeroize();
            }
        }
    }
}

impl Snapshot {
    fn new(credentials: &ApiCredentials) -> Self {
        let data = SessionData::default();
        Self {
            api_id: credentials.api_id,
            api_hash: credentials.api_hash().to_owned(),
            home_dc: data.home_dc,
            dc_options: data.dc_options.into_values().collect(),
            peers: Vec::new(),
            updates: UpdatesState::default(),
        }
    }

    fn merge_peers(&mut self, peers: impl IntoIterator<Item = PeerInfo>) {
        for peer in peers {
            if let Some(existing) = self.peers.iter_mut().find(|info| info.id() == peer.id()) {
                existing.extend_info(&peer);
            } else {
                self.peers.push(peer);
            }
        }
    }
}

struct Inner {
    state: Mutex<Snapshot>,
    key: SecretKey,
    vault: Arc<Vault>,
}

pub(crate) struct SecureSession {
    inner: Arc<Inner>,
}

impl SecureSession {
    pub fn restore(vault: Arc<Vault>) -> Result<Option<Arc<Self>>, VaultError> {
        let Some((state, key)) = vault.read::<Snapshot>()? else {
            return Ok(None);
        };
        ApiCredentials::new(state.api_id, state.api_hash.clone())
            .map_err(|_| VaultError::Corrupt)?;
        Ok(Some(Arc::new(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                key,
                vault,
            }),
        })))
    }

    pub fn configure(
        vault: Arc<Vault>,
        credentials: ApiCredentials,
    ) -> Result<Arc<Self>, VaultError> {
        let key = vault.key_for_configuration::<Snapshot>()?;
        let state = Snapshot::new(&credentials);
        vault.write(&state, &key)?;
        Ok(Arc::new(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                key,
                vault,
            }),
        }))
    }

    pub fn credentials(&self) -> Result<ApiCredentials, VaultError> {
        let state = self.inner.state.lock().map_err(|_| VaultError::Io)?;
        ApiCredentials::new(state.api_id, state.api_hash.clone()).map_err(|_| VaultError::Corrupt)
    }

    pub fn account_id(&self) -> Result<Option<i64>, VaultError> {
        let state = self.inner.state.lock().map_err(|_| VaultError::Io)?;
        Ok(state.peers.iter().find_map(|peer| match peer {
            PeerInfo::User {
                id,
                is_self: Some(true),
                ..
            } if *id > 0 => Some(*id),
            _ => None,
        }))
    }

    async fn change(
        &self,
        change: impl FnOnce(&mut Snapshot) + Send + 'static,
    ) -> Result<(), VaultError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = inner.state.lock().map_err(|_| VaultError::Io)?;
            let mut next = state.clone();
            change(&mut next);
            // GetDialogs returns cached peers/PTS repeatedly. Persist real
            // changes, not an identical encrypted file for every known dialog.
            if next == *state {
                return Ok(());
            }
            inner.vault.write(&next, &inner.key)?;
            *state = next;
            Ok(())
        })
        .await
        .map_err(|_| VaultError::Io)?
    }

    pub async fn reset_authorization(&self) -> Result<(), VaultError> {
        let credentials = self.credentials()?;
        self.change(move |state| *state = Snapshot::new(&credentials))
            .await
    }
}

impl Session for SecureSession {
    type Error = VaultError;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        Ok(self.inner.state.lock().map_err(|_| VaultError::Io)?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(self.change(move |state| state.home_dc = dc_id))
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| VaultError::Io)?
            .dc_options
            .iter()
            .find(|dc| dc.id == dc_id)
            .cloned())
    }

    fn set_dc_option(&self, option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        let option = option.clone();
        Box::pin(self.change(move |state| {
            if let Some(existing) = state.dc_options.iter_mut().find(|dc| dc.id == option.id) {
                *existing = option;
            } else {
                state.dc_options.push(option);
            }
        }))
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move {
            Ok(self
                .inner
                .state
                .lock()
                .map_err(|_| VaultError::Io)?
                .peers
                .iter()
                .find(|info| {
                    if peer == PeerId::self_user() {
                        matches!(
                            info,
                            PeerInfo::User {
                                is_self: Some(true),
                                ..
                            }
                        )
                    } else {
                        info.id() == peer
                    }
                })
                .cloned())
        })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(self.change(move |state| state.merge_peers([peer])))
    }

    fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(self.change(move |state| state.merge_peers(peers)))
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async move {
            Ok(self
                .inner
                .state
                .lock()
                .map_err(|_| VaultError::Io)?
                .updates
                .clone())
        })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(self.change(move |state| {
            match update {
                UpdateState::All(updates) => state.updates = updates,
                UpdateState::Primary { pts, date, seq } => {
                    state.updates.pts = pts;
                    state.updates.date = date;
                    state.updates.seq = seq;
                }
                UpdateState::Secondary { qts } => state.updates.qts = qts,
                UpdateState::Channel { id, pts } => {
                    if let Some(channel) = state
                        .updates
                        .channels
                        .iter_mut()
                        .find(|channel| channel.id == id)
                    {
                        channel.pts = pts;
                    } else {
                        state.updates.channels.push(ChannelState { id, pts });
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::vault::tests::MemoryKeys;

    use super::*;

    #[tokio::test]
    async fn permanent_mtproto_keys_and_credentials_survive_without_a_shutdown_flush()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let keys = Arc::new(MemoryKeys::default());
        let vault = Arc::new(Vault::open(directory.path(), keys.clone())?);
        let session =
            SecureSession::configure(vault.clone(), ApiCredentials::new(1, "0".repeat(32))?)?;
        let Some(mut dc) = session.dc_option(2)? else {
            panic!("known DC missing")
        };
        dc.auth_key = Some([42; 256]);
        session.set_dc_option(&dc).await?;
        session.set_home_dc_id(2).await?;
        let ciphertext = std::fs::read(vault.path())?;
        assert!(!String::from_utf8_lossy(&ciphertext).contains(&"0".repeat(32)));
        drop(session);
        drop(vault);
        let vault = Arc::new(Vault::open(directory.path(), keys)?);
        let Some(restored) = SecureSession::restore(vault)? else {
            panic!("session missing")
        };
        assert_eq!(restored.home_dc_id()?, 2);
        assert_eq!(
            restored.dc_option(2)?.and_then(|dc| dc.auth_key),
            Some([42; 256])
        );
        assert_eq!(restored.credentials()?.api_hash(), "0".repeat(32));
        Ok(())
    }

    #[tokio::test]
    async fn failed_write_does_not_publish_an_unpersisted_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let vault = Arc::new(Vault::open(
            directory.path(),
            Arc::new(MemoryKeys::default()),
        )?);
        let session =
            SecureSession::configure(vault.clone(), ApiCredentials::new(1, "0".repeat(32))?)?;
        let original = session.home_dc_id()?;
        std::fs::remove_file(vault.path())?;
        std::fs::create_dir(vault.path())?;
        assert!(session.set_home_dc_id(99).await.is_err());
        assert_eq!(session.home_dc_id()?, original);
        Ok(())
    }

    #[tokio::test]
    async fn batch_peer_updates_are_durable_and_identical_snapshots_are_not_rewritten()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let vault = Arc::new(Vault::open(
            directory.path(),
            Arc::new(MemoryKeys::default()),
        )?);
        let session =
            SecureSession::configure(vault.clone(), ApiCredentials::new(1, "0".repeat(32))?)?;
        let peers: Vec<_> = (1..=100)
            .map(|id| PeerInfo::Channel {
                id,
                auth: None,
                kind: None,
            })
            .collect();
        session.cache_peers(peers.clone()).await?;
        let ciphertext = std::fs::read(vault.path())?;
        session.cache_peers(peers).await?;
        session.set_home_dc_id(session.home_dc_id()?).await?;
        assert_eq!(std::fs::read(vault.path())?, ciphertext);
        let Some(restored) = SecureSession::restore(vault.clone())? else {
            panic!("missing snapshot")
        };
        let Some(id) = PeerId::channel(100) else {
            panic!("invalid test id")
        };
        assert!(matches!(
            restored.peer(id).await?,
            Some(PeerInfo::Channel { id: 100, .. })
        ));
        session
            .set_update_state(UpdateState::Channel { id: 100, pts: 10 })
            .await?;
        session
            .set_update_state(UpdateState::Channel { id: 200, pts: 20 })
            .await?;
        let ciphertext = std::fs::read(vault.path())?;
        session
            .set_update_state(UpdateState::Channel { id: 100, pts: 10 })
            .await?;
        assert_eq!(std::fs::read(vault.path())?, ciphertext);
        Ok(())
    }

    #[tokio::test]
    async fn self_peer_and_update_state_are_restored_without_exporting_grammers_types()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let keys = Arc::new(MemoryKeys::default());
        let vault = Arc::new(Vault::open(directory.path(), keys.clone())?);
        let session = SecureSession::configure(vault, ApiCredentials::new(1, "0".repeat(32))?)?;
        assert_eq!(session.account_id()?, None);
        session
            .cache_peer(PeerInfo::User {
                id: 42,
                auth: None,
                bot: Some(false),
                is_self: Some(true),
            })
            .await?;
        session
            .set_update_state(UpdateState::Secondary { qts: 17 })
            .await?;
        drop(session);
        let vault = Arc::new(Vault::open(directory.path(), keys)?);
        let Some(restored) = SecureSession::restore(vault)? else {
            panic!("missing session")
        };
        assert!(matches!(
            restored.peer(PeerId::self_user()).await?,
            Some(PeerInfo::User { id: 42, .. })
        ));
        assert_eq!(restored.updates_state().await?.qts, 17);
        assert_eq!(restored.account_id()?, Some(42));
        restored.reset_authorization().await?;
        assert_eq!(restored.account_id()?, None);
        Ok(())
    }
}
