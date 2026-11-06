use crate::{
    AuthStep, Authentication, BookFormat, CatalogError, CatalogState, ChannelCatalog, CoreEvent,
    MonitorCursor, MonitorPage, Recommendation, RecommendationError as Error, RecommendationState,
    RecommendationStatus, RecommendationStore, SCAN_PAGE_SIZE, auth::AuthController,
};
use async_channel::Sender;
use std::time::Duration;
use tokio::time::Instant;

pub(super) struct Monitor {
    pub state: RecommendationState,
    pub next: Instant,
    source_index: usize,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            state: RecommendationState::default(),
            next: Instant::now(),
            source_index: 0,
        }
    }
}

impl Monitor {
    pub fn due(&self) -> bool {
        self.state.monitoring && self.next <= Instant::now()
    }

    pub async fn send(&self, events: &Sender<CoreEvent>) -> bool {
        events
            .send(CoreEvent::Recommendations(self.state.clone()))
            .await
            .is_ok()
    }

    pub async fn sync(
        &mut self,
        store: &impl RecommendationStore,
        auth: &AuthController<impl Authentication>,
        catalog: &CatalogState,
        force: bool,
    ) -> bool {
        let account = auth.account_id().filter(|id| *id > 0);
        if self.state.account != account {
            self.state = RecommendationState {
                account,
                ..Default::default()
            };
            self.source_index = 0;
        } else if !force && (self.state.loaded || self.state.error.is_some() || account.is_none()) {
            return false;
        }
        if let Some(account) = account {
            let restore = !self.state.loaded;
            let loaded = async {
                let items = store.load_recommendations(account).await?;
                let enabled = if restore {
                    store.load_monitoring_enabled(account).await?
                } else {
                    false
                };
                Ok::<_, Error>((items, enabled))
            }
            .await;
            match loaded {
                Ok((items, enabled)) => {
                    self.state.items = items;
                    self.state.loaded = true;
                    self.state.error = None;
                    if restore {
                        self.enable(enabled, auth, catalog);
                    }
                }
                Err(error) => {
                    self.state.error = Some(error);
                    self.state.monitoring = false;
                }
            }
        }
        true
    }

    pub async fn set_enabled(
        &mut self,
        store: &impl RecommendationStore,
        enabled: bool,
        auth: &AuthController<impl Authentication>,
        catalog: &CatalogState,
    ) {
        self.enable(enabled, auth, catalog);
        if self.state.error.is_some() {
            return;
        }
        let result = match self.state.account {
            Some(account) => store.save_monitoring_enabled(account, enabled).await,
            None => Err(Error::Unauthorized),
        };
        if let Err(error) = result {
            self.state.monitoring = false;
            self.state.error = Some(error);
        }
    }

    pub fn enable(
        &mut self,
        enabled: bool,
        auth: &AuthController<impl Authentication>,
        catalog: &CatalogState,
    ) {
        self.state.error = None;
        self.state.monitoring = false;
        if !enabled {
            return;
        }
        self.state.error = if auth.state().step != AuthStep::Authorized
            || self.state.account != auth.account_id()
        {
            Some(Error::Unauthorized)
        } else if !self.state.loaded || !catalog.sources.loaded {
            Some(Error::Storage)
        } else if catalog.sources.channels.is_empty() {
            Some(Error::NoSources)
        } else {
            auth.remaining_wait()
                .map(|seconds| Error::Telegram(CatalogError::FloodWait(seconds)))
        };
        if self.state.error.is_none() {
            self.state.monitoring = true;
            self.next = Instant::now();
            self.source_index = 0;
        }
    }

    pub async fn poll(
        &mut self,
        store: &impl RecommendationStore,
        auth: &mut AuthController<impl Authentication + ChannelCatalog>,
        catalog: &mut CatalogState,
        events: &Sender<CoreEvent>,
    ) -> bool {
        if !self.state.monitoring {
            return true;
        }
        let count = catalog.sources.channels.len();
        if count == 0 {
            self.state.monitoring = false;
            self.state.error = Some(Error::NoSources);
            return self.send(events).await;
        }
        let source = catalog.sources.channels[self.source_index % count].clone();
        self.source_index = (self.source_index + 1) % count;
        let delay = if self.source_index == 0 {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(1)
        };
        self.state.checking = Some(source.title.clone());
        if !self.send(events).await {
            return false;
        }
        let result = self.poll_source(store, auth, catalog, source.id).await;
        let mut inserted = Vec::new();
        self.next = Instant::now() + delay;
        self.state.checking = None;
        if let Err(error) = result {
            self.state.error = Some(error);
            // Inaccessible/protected sources do not prevent checking the others.
            if !matches!(
                error,
                Error::Telegram(CatalogError::Unavailable | CatalogError::ProtectedContent)
            ) {
                self.state.monitoring = false;
                if let Some(account) = self.state.account
                    && store.save_monitoring_enabled(account, false).await.is_err()
                {
                    self.state.error = Some(Error::Storage);
                }
            }
            if let Error::Telegram(CatalogError::FloodWait(seconds)) = error {
                auth.record_flood_wait(seconds);
            }
            if error == Error::Telegram(CatalogError::Unauthorized)
                && !super::catalog_failure(auth, catalog, CatalogError::Unauthorized, events).await
            {
                return false;
            }
            if error == Error::Telegram(CatalogError::Unauthorized) {
                // Re-authentication may restore a different account. Never send
                // the previous owner's inbox after publishing its replacement.
                self.sync(store, auth, catalog, false).await;
            }
        } else if let Ok(keys) = result {
            inserted = keys;
            self.state.error = None;
        }
        if !self.send(events).await {
            return false;
        }
        if !inserted.is_empty()
            && let Some(account) = self.state.account
        {
            return events
                .send(CoreEvent::RecommendationBatch(crate::RecommendationBatch {
                    account,
                    keys: inserted,
                }))
                .await
                .is_ok();
        }
        true
    }

    async fn poll_source(
        &mut self,
        store: &impl RecommendationStore,
        auth: &mut AuthController<impl Authentication + ChannelCatalog>,
        catalog: &CatalogState,
        channel: i64,
    ) -> Result<Vec<crate::DownloadKey>, Error> {
        let account = self.state.account.ok_or(Error::Unauthorized)?;
        if auth.account_id() != Some(account) {
            return Err(Error::Unauthorized);
        }
        if let Some(seconds) = auth.remaining_wait() {
            return Err(Error::Telegram(CatalogError::FloodWait(seconds)));
        }
        let source = catalog
            .channels
            .iter()
            .find(|source| source.id == channel)
            .ok_or(Error::Telegram(CatalogError::Unavailable))?;
        if source.protected {
            return Err(Error::Telegram(CatalogError::ProtectedContent));
        }
        let cursor = store.load_monitor_cursor(account, channel).await?;
        let page = auth
            .backend_mut()
            .scan_updates(channel, cursor.and_then(|cursor| cursor.before))
            .await
            .map_err(Error::Telegram)?;
        let (cursor, items) = process_page(channel, &source.title, catalog.formats, cursor, page)?;
        let inserted = store
            .commit_recommendations(account, channel, cursor, &items)
            .await?;
        self.state.items = store.load_recommendations(account).await?;
        Ok(inserted)
    }
}

/// Validate adapter output and freeze a high watermark while walking a backlog.
/// The first page establishes a baseline; no historical books become suggestions.
pub(super) fn process_page(
    channel: i64,
    title: &str,
    formats: crate::BookFormats,
    cursor: Option<MonitorCursor>,
    page: MonitorPage,
) -> Result<(MonitorCursor, Vec<Recommendation>), Error> {
    let invalid = || Error::Telegram(CatalogError::InvalidResponse);
    let before = cursor.and_then(|cursor| cursor.before);
    if cursor.is_some_and(|cursor| !cursor.valid())
        || page.scanned as usize > SCAN_PAGE_SIZE
        || page.skipped_protected > page.scanned
        || page.books.len() > page.scanned as usize
        || (page.scanned == 0) != page.newest.is_none()
        || page
            .newest
            .is_some_and(|id| id <= 0 || before.is_some_and(|before| id >= before))
        || page.next_offset.is_some_and(|id| {
            id <= 0
                || page.scanned as usize != SCAN_PAGE_SIZE
                || before.is_some_and(|before| id >= before)
                || page.newest.is_none_or(|newest| id > newest)
        })
        || page.books.iter().any(|found| {
            found.book.key.channel_id != channel
                || found.book.key.message_id <= 0
                || page
                    .newest
                    .is_none_or(|newest| found.book.key.message_id > newest)
                || before.is_some_and(|before| found.book.key.message_id >= before)
                || BookFormat::from_file_name(&found.book.file_name) != Some(found.book.format)
        })
    {
        return Err(invalid());
    }
    let Some(cursor) = cursor else {
        return Ok((
            MonitorCursor {
                after: page.newest.unwrap_or(0),
                before: None,
                target: None,
            },
            Vec::new(),
        ));
    };
    let target = cursor
        .target
        .unwrap_or(cursor.after.max(page.newest.unwrap_or(0)));
    let items = page
        .books
        .into_iter()
        .filter(|found| {
            found.book.key.message_id > cursor.after
                && found.book.key.message_id <= target
                && formats.contains(found.book.format)
        })
        .map(|found| Recommendation {
            book: found.book,
            source_title: title.into(),
            message_date: found.message_date,
            status: RecommendationStatus::Unread,
        })
        .collect();
    let next = match page.next_offset.filter(|before| *before > cursor.after) {
        Some(before) => MonitorCursor {
            before: Some(before),
            target: Some(target),
            ..cursor
        },
        None => MonitorCursor {
            after: target,
            before: None,
            target: None,
        },
    };
    if !next.valid() {
        return Err(invalid());
    }
    Ok((next, items))
}

#[cfg(test)]
#[path = "monitor_service_tests.rs"]
mod tests;
