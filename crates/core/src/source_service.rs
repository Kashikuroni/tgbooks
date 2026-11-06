use crate::{
    AuthStep, Authentication, CatalogState, SavedSources, SourceError, SourceStore,
    auth::AuthController,
};

pub(super) async fn sync(
    store: &impl SourceStore,
    auth: &AuthController<impl Authentication>,
    catalog: &mut CatalogState,
    force: bool,
) -> bool {
    let account = auth.account_id();
    if auth.state().step != AuthStep::Authorized {
        let changed = catalog.sources != SavedSources::default();
        catalog.sources = SavedSources::default();
        return changed;
    }
    if !force
        && catalog.sources.account == account
        && (catalog.sources.loaded || catalog.sources.error.is_some())
    {
        return false;
    }
    if catalog.sources.account != account {
        // Clear both catalogue and saved metadata before publishing another owner.
        catalog.clear_authorization(None);
        catalog.authenticated = true;
        catalog.sources.account = account;
    }
    let result = match account.filter(|account| *account > 0) {
        Some(account) => store.load_sources(account).await,
        None => Err(SourceError::Unauthorized),
    };
    match result {
        Ok(channels) => {
            catalog.sources.channels = channels;
            catalog.sources.loaded = true;
            catalog.sources.error = None;
        }
        Err(error) => catalog.sources.error = Some(error),
    }
    true
}

pub(super) async fn set(
    store: &impl SourceStore,
    auth: &AuthController<impl Authentication>,
    catalog: &mut CatalogState,
    id: i64,
    enabled: bool,
) -> Result<(), SourceError> {
    let account = auth
        .account_id()
        .filter(|id| *id > 0)
        .ok_or(SourceError::Unauthorized)?;
    if auth.state().step != AuthStep::Authorized || catalog.sources.account != Some(account) {
        return Err(SourceError::Unauthorized);
    }
    if !catalog.sources.loaded {
        return Err(SourceError::Storage);
    }
    let channel = if enabled {
        catalog.channels.iter().find(|channel| channel.id == id)
    } else {
        catalog.channel(id)
    }
    .cloned()
    .ok_or(SourceError::Unavailable)?;
    store.set_source(account, &channel, enabled).await?;
    catalog.sources.channels.retain(|channel| channel.id != id);
    if enabled {
        catalog.sources.channels.push(channel);
    }
    catalog.sources.error = None;
    if catalog.selected == Some(id) && catalog.channel(id).is_none() {
        let _ = catalog.select(None);
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_service_tests.rs"]
mod tests;
