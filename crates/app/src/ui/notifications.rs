//! Native presentation policy. Core only reports newly committed Telegram identities.
use gpui::{SystemNotification, SystemNotificationAction, SystemNotificationResponse};
use std::collections::HashSet;
use tgbooks_core::{DownloadKey, RecommendationBatch};

pub(super) const TAG: &str = "tgbooks-new-books";
pub(super) const OPEN: &str = "open-recommendations";
pub(super) const GROUP_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Notifications {
    pub account: Option<i64>,
    pending: HashSet<DownloadKey>,
    seen: HashSet<DownloadKey>,
    posted: bool,
}

impl Notifications {
    pub fn set_account(&mut self, account: Option<i64>) -> bool {
        if self.account == account {
            return false;
        }
        self.account = account;
        self.clear();
        self.seen.clear();
        true
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.posted = false;
    }
    pub fn add(&mut self, batch: RecommendationBatch) -> bool {
        if self.account != Some(batch.account) {
            return false;
        }
        for key in batch.keys {
            if self.seen.insert(key) {
                self.pending.insert(key);
            }
        }
        !self.pending.is_empty()
    }
    pub fn take(
        &mut self,
        eligible: &HashSet<DownloadKey>,
        active: bool,
        enabled: bool,
    ) -> Option<SystemNotification> {
        let keys: Vec<_> = self
            .pending
            .drain()
            .filter(|key| eligible.contains(key))
            .collect();
        if active || !enabled || keys.is_empty() {
            return None;
        }
        let channels = keys
            .iter()
            .map(|key| key.channel_id)
            .collect::<HashSet<_>>()
            .len();
        self.posted = true;
        Some(SystemNotification {
            tag: TAG.into(),
            title: "Новые книги".into(),
            body: format!(
                "{} · {}. Откройте список, чтобы выбрать книги.",
                super::files::count(keys.len(), "книга", "книги", "книг"),
                super::files::count(channels, "канал", "канала", "каналов")
            )
            .into(),
            actions: vec![SystemNotificationAction {
                id: OPEN.into(),
                label: "Открыть".into(),
            }],
        })
    }
    pub fn accepts(&self, response: &SystemNotificationResponse, current: Option<i64>) -> bool {
        self.posted
            && self.account.is_some()
            && self.account == current
            && response.tag == TAG
            && response.action_id.as_ref().is_none_or(|id| id == OPEN)
    }
}

pub(super) fn register(window: gpui::WindowHandle<super::BookApp>, cx: &gpui::App) {
    cx.on_system_notification_response(move |response, cx| {
        let _ = window.update(cx, |app, window, cx| {
            if app
                .notifications
                .accepts(&response, app.recommendations.read(cx).account())
            {
                app.show_recommendations = true;
                app.navigate(super::Screen::Channels, cx);
                window.activate_window();
                cx.activate(true);
                app.clear_notifications(cx);
            }
        });
    });
}

impl super::BookApp {
    pub(super) fn clear_notifications(&mut self, cx: &mut gpui::Context<Self>) {
        self.notification_timer = None;
        self.notifications.clear();
        if self.notification_available {
            cx.dismiss_system_notification(TAG);
        }
    }

    pub(super) fn notification_account(
        &mut self,
        account: Option<i64>,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.notifications.set_account(account) {
            self.notification_timer = None;
            if self.notification_available {
                cx.dismiss_system_notification(TAG);
            }
        }
    }

    pub(super) fn queue_notification(
        &mut self,
        batch: RecommendationBatch,
        cx: &mut gpui::Context<Self>,
    ) {
        if !self.notifications.add(batch) {
            return;
        }
        if !self.notification_available
            || !self
                .info
                .as_ref()
                .is_some_and(|info| info.settings.system_notifications)
        {
            self.clear_notifications(cx);
            return;
        }
        if self.notification_timer.is_some() {
            return;
        }
        let Some(window) = self.notification_window else {
            self.clear_notifications(cx);
            return;
        };
        let timer = cx.background_executor().timer(GROUP_DELAY);
        self.notification_timer = Some(cx.spawn(async move |_, cx| {
            timer.await;
            let _ = window.update(cx, |app, window, cx| {
                app.notification_timer = None;
                app.flush_notifications(window, cx);
            });
        }));
    }

    pub(super) fn flush_notifications(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let eligible = self.recommendations.read(cx).notification_keys();
        let enabled = self.notification_available
            && self
                .info
                .as_ref()
                .is_some_and(|info| info.settings.system_notifications);
        if let Some(notification) =
            self.notifications
                .take(&eligible, window.is_window_active(), enabled)
        {
            cx.show_system_notification(notification);
        }
    }

    pub(super) fn test_notification(&self, cx: &mut gpui::Context<Self>) {
        // An explicit diagnostic click is consent for this one notification,
        // independent of the persisted opt-in for automatic notifications.
        if self.notification_available {
            cx.show_system_notification(SystemNotification {
                tag: "tgbooks-notification-test".into(),
                title: "Telegram Books".into(),
                body:
                    "Проверка системных уведомлений. Их показ зависит от разрешений и настроек ОС."
                        .into(),
                actions: Vec::new(),
            });
        }
    }
}
