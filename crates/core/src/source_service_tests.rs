use super::*;
use crate::{AuthAction, AuthFailure, ChannelSummary};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Account(i64);
impl Authentication for Account {
    fn account_id(&self) -> Option<i64> {
        Some(self.0)
    }
    async fn execute(&mut self, _: AuthAction) -> Result<AuthStep, AuthFailure> {
        Ok(AuthStep::Authorized)
    }
}
fn auth(owner: i64) -> AuthController<Account> {
    let mut auth = AuthController::new(Account(owner));
    auth.complete(Ok(AuthStep::Authorized));
    auth
}
fn channel(id: i64) -> ChannelSummary {
    ChannelSummary {
        id,
        title: format!("Канал {id}"),
        username: None,
        protected: false,
    }
}
#[derive(Default)]
struct Store {
    rows: Mutex<HashMap<i64, Vec<ChannelSummary>>>,
    fail_load: AtomicBool,
    fail_save: AtomicBool,
    loads: AtomicUsize,
}
impl SourceStore for Store {
    async fn load_sources(&self, account: i64) -> Result<Vec<ChannelSummary>, SourceError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        if self.fail_load.load(Ordering::SeqCst) {
            return Err(SourceError::Storage);
        }
        Ok(self
            .rows
            .lock()
            .map_err(|_| SourceError::Storage)?
            .get(&account)
            .cloned()
            .unwrap_or_default())
    }
    async fn set_source(
        &self,
        account: i64,
        channel: &ChannelSummary,
        enabled: bool,
    ) -> Result<(), SourceError> {
        if self.fail_save.load(Ordering::SeqCst) {
            return Err(SourceError::Storage);
        }
        let mut rows = self.rows.lock().map_err(|_| SourceError::Storage)?;
        let entries = rows.entry(account).or_default();
        entries.retain(|row| row.id != channel.id);
        if enabled {
            entries.push(channel.clone());
        }
        Ok(())
    }
}

#[tokio::test]
async fn durable_acknowledgement_does_not_select_scan_or_erase_sources_on_failure() {
    let store = Store::default();
    let auth = auth(1);
    let mut catalog = CatalogState::default();
    sync(&store, &auth, &mut catalog, false).await;
    catalog.replace_channels(vec![channel(-1)]);
    assert_eq!(set(&store, &auth, &mut catalog, -1, true).await, Ok(()));
    assert!(catalog.sources.contains(-1));
    assert_eq!(catalog.selected, None);
    assert!(!catalog.scan_started && catalog.books.is_empty() && catalog.operation.is_none());
    store.fail_save.store(true, Ordering::SeqCst);
    assert_eq!(
        set(&store, &auth, &mut catalog, -1, false).await,
        Err(SourceError::Storage)
    );
    assert!(catalog.sources.contains(-1));
    store.fail_load.store(true, Ordering::SeqCst);
    sync(&store, &auth, &mut catalog, true).await;
    assert!(catalog.sources.contains(-1));
    assert!(
        !sync(&store, &auth, &mut catalog, false).await,
        "failed loads must not retry in a loop"
    );
}

#[tokio::test]
async fn missing_and_protected_sources_remain_removable_but_cannot_scan() {
    let store = Store::default();
    let auth = auth(1);
    let mut catalog = CatalogState::default();
    sync(&store, &auth, &mut catalog, false).await;
    catalog.replace_channels(vec![channel(-1)]);
    assert_eq!(set(&store, &auth, &mut catalog, -1, true).await, Ok(()));
    let mut renamed = channel(-1);
    renamed.title = "Новое имя".into();
    renamed.protected = true;
    catalog.replace_channels(vec![renamed.clone()]);
    assert_eq!(catalog.source_channels(), vec![&renamed]);
    assert_eq!(catalog.select(Some(-1)), Ok(()));
    assert!(!catalog.can_scan());
    catalog.replace_channels(Vec::new());
    assert_eq!(catalog.selected, Some(-1));
    assert!(!catalog.can_scan());
    assert_eq!(
        set(&store, &auth, &mut catalog, -2, true).await,
        Err(SourceError::Unavailable)
    );
    assert_eq!(set(&store, &auth, &mut catalog, -1, false).await, Ok(()));
    assert!(catalog.sources.channels.is_empty());
    assert_eq!(catalog.selected, None);
}

#[tokio::test]
async fn changing_accounts_or_revoking_auth_never_exposes_previous_sources() {
    let store = Store::default();
    let first = auth(1);
    let second = auth(2);
    let mut catalog = CatalogState::default();
    sync(&store, &first, &mut catalog, false).await;
    catalog.replace_channels(vec![channel(-1)]);
    assert_eq!(set(&store, &first, &mut catalog, -1, true).await, Ok(()));
    store.fail_load.store(true, Ordering::SeqCst);
    sync(&store, &second, &mut catalog, false).await;
    assert!(catalog.sources.channels.is_empty() && catalog.channels.is_empty());
    assert_eq!(catalog.sources.account, Some(2));
    assert!(!catalog.sources.loaded);
    assert_eq!(
        set(&store, &first, &mut catalog, -1, false).await,
        Err(SourceError::Unauthorized)
    );
    let loads = store.loads.load(Ordering::SeqCst);
    sync(&store, &second, &mut catalog, false).await;
    assert_eq!(store.loads.load(Ordering::SeqCst), loads);
    store.fail_load.store(false, Ordering::SeqCst);
    sync(&store, &second, &mut catalog, true).await;
    assert!(catalog.sources.loaded && catalog.sources.channels.is_empty());
    sync(&store, &first, &mut catalog, false).await;
    assert!(catalog.sources.contains(-1));
    let unauthenticated = AuthController::new(Account(1));
    sync(&store, &unauthenticated, &mut catalog, false).await;
    assert_eq!(catalog.sources, SavedSources::default());
}
