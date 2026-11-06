mod channels;
mod chrome;
mod components;
mod downloads;
mod files;
mod icons;
mod input;
mod install;
mod library;
mod login;
mod metrics;
mod notifications;
mod recommendations;
mod select;
mod settings;
mod theme;

#[cfg(feature = "gpui-tests")]
pub mod preview;

use std::{cell::Cell, future::Future, rc::Rc};

use anyhow::{Context as _, Result};
use async_channel::Sender;
use gpui::{
    AnyElement, App, Bounds, Context, Div, FocusHandle, KeyBinding, KeyDownEvent, Menu, MenuItem,
    Role, SharedString, Task, Window, actions, div, prelude::*, px, size,
};
use tgbooks_core::{
    AppInfo, AuthState, AuthStep, CatalogState, CoreCommand, CoreEvent, DownloadRecord,
    DownloadStatus, ServiceFailure, TransferState, UiConnection,
};

use crate::platform;
use components::{button, icon_button, navigation_button};
use icons::{Icon, IconAssets};
use theme::{SIDEBAR_WIDTH, Theme};

actions!(
    tgbooks,
    [
        Quit,
        FocusNext,
        FocusPrevious,
        Login,
        Channels,
        Downloads,
        Library,
        Settings,
        Refresh,
        ToggleSidebar
    ]
);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Screen {
    #[default]
    Login,
    Channels,
    Downloads,
    Library,
    Settings,
}

impl Screen {
    const ALL: [Self; 5] = [
        Self::Login,
        Self::Channels,
        Self::Downloads,
        Self::Library,
        Self::Settings,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Login => "Вход",
            Self::Channels => "Каналы",
            Self::Downloads => "Загрузки",
            Self::Library => "Библиотека",
            Self::Settings => "Настройки",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Login => "Ваш личный Telegram-аккаунт",
            Self::Channels => "Источники вашей библиотеки",
            Self::Downloads => "Очередь и история загрузок",
            Self::Library => "Скачанные книги на этом компьютере",
            Self::Settings => "Локальные данные приложения",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Login => Icon::Account,
            Self::Channels => Icon::Channels,
            Self::Downloads => Icon::Downloads,
            Self::Library => Icon::Library,
            Self::Settings => Icon::Settings,
        }
    }
}

pub fn run(
    connection: UiConnection,
    shutdown: impl Future<Output = ()> + 'static,
    start: Sender<()>,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    let window_error = Rc::new(Cell::new(None));
    let error = window_error.clone();
    gpui_platform::application()
        .with_assets(IconAssets)
        .run(move |cx: &mut App| {
            cx.set_app_identity(platform::APP_ID, platform::APP_NAME);
            tracing::info!("opening native window");
            let mut shutdown = Some(shutdown);
            cx.on_app_quit(move |_| {
                let shutdown = shutdown.take();
                async move {
                    if let Some(shutdown) = shutdown {
                        shutdown.await;
                    }
                }
            })
            .detach();
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus([Menu::new("Telegram Books")
                .items([MenuItem::action("Завершить Telegram Books", Quit)])]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let window =
                match cx.open_window(platform::window_options(bounds), move |window, cx| {
                    let view = cx.new(|cx| BookApp::new(connection, cx));
                    view.read(cx).focus_handle.clone().focus(window, cx);
                    view
                }) {
                    Ok(window) => window,
                    Err(failure) => {
                        error.set(Some(failure));
                        cx.quit();
                        return;
                    }
                };
            let available = platform::notification_support(cx);
            let _ = window.update(cx, |app, _, cx| {
                app.notification_available = available;
                app.preferences.update(cx, |view, cx| {
                    view.set_notification_available(available, cx)
                });
            });
            notifications::register(window, cx);
            cx.activate(true);
            tracing::info!("native window ready");
            install::offer(window, start, runtime, cx);
        });
    match window_error.take() {
        Some(error) => Err(error).context("could not open the native window"),
        None => Ok(()),
    }
}

fn bind_keys(cx: &mut App) {
    use platform::shortcut;

    input::bind_keys(cx);
    login::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new(shortcut("cmd-q", "ctrl-q"), Quit, None),
        KeyBinding::new(shortcut("cmd-1", "ctrl-1"), Login, None),
        KeyBinding::new(shortcut("cmd-2", "ctrl-2"), Channels, None),
        KeyBinding::new(shortcut("cmd-3", "ctrl-3"), Downloads, None),
        KeyBinding::new(shortcut("cmd-4", "ctrl-4"), Library, None),
        KeyBinding::new(shortcut("cmd-5", "ctrl-5"), Settings, None),
        KeyBinding::new(shortcut("cmd-r", "ctrl-r"), Refresh, None),
        KeyBinding::new(shortcut("cmd-b", "ctrl-b"), ToggleSidebar, None),
    ]);
}

struct BookApp {
    screen: Screen,
    focus_handle: FocusHandle,
    sidebar_toggle_focus: FocusHandle,
    cancel_focus: FocusHandle,
    queue_focus: FocusHandle,
    download_scroll: gpui::ScrollHandle,
    download_controls: std::collections::HashMap<downloads::Control, downloads::ScrollControl>,
    focus_download: bool,
    sidebar_open: bool,
    commands: Sender<CoreCommand>,
    info: Option<AppInfo>,
    records: Vec<DownloadRecord>,
    transfer: TransferState,
    metrics: Option<(tgbooks_core::DownloadKey, metrics::DisplayMetrics)>,
    loading: bool,
    notice: Option<&'static str>,
    authentication: AuthState,
    login: gpui::Entity<login::LoginForm>,
    catalog: gpui::Entity<channels::CatalogView>,
    library: gpui::Entity<library::FileTable>,
    history: gpui::Entity<library::FileTable>,
    preferences: gpui::Entity<settings::SettingsView>,
    recommendations: gpui::Entity<recommendations::RecommendationsView>,
    show_recommendations: bool,
    _recommendation_events: gpui::Subscription,
    _catalog_events: gpui::Subscription,
    notifications: notifications::Notifications,
    notification_timer: Option<Task<()>>,
    notification_window: Option<gpui::WindowHandle<BookApp>>,
    notification_available: bool,
    _notification_test_events: gpui::Subscription,
    show_history: bool,
    _library_events: gpui::Subscription,
    _history_events: gpui::Subscription,
    _events: Task<()>,
}

impl BookApp {
    fn new(connection: UiConnection, cx: &mut Context<Self>) -> Self {
        let events = connection.events;
        let task = cx.spawn(async move |view, cx| {
            while let Ok(event) = events.recv().await {
                if view
                    .update(cx, |view, cx| {
                        view.apply_event(event, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = view.update(cx, |view, cx| {
                view.loading = false;
                view.notice
                    .get_or_insert("Фоновый сервис остановлен. Перезапустите приложение.");
                if view.authentication.busy || view.authentication.step == AuthStep::Authorized {
                    view.authentication = AuthState {
                        error: Some(tgbooks_core::AuthError::Network),
                        ..Default::default()
                    };
                    view.login.update(cx, |form, cx| {
                        form.set_state(view.authentication.clone(), cx)
                    });
                }
                view.catalog.update(cx, |catalog, cx| {
                    catalog.set_state(CatalogState::default(), cx);
                    catalog.set_transfer(TransferState::default(), cx);
                });
                view.transfer = TransferState {
                    error: view
                        .transfer
                        .active
                        .map(|_| tgbooks_core::DownloadError::Network),
                    ..Default::default()
                };
                view.metrics = None;
                view.notification_account(None, cx);
                view.preferences.update(cx, |view, cx| view.stop(cx));
                view.recommendations
                    .update(cx, |view, cx| view.set_state(Default::default(), cx));
                view.sync_file_tables(cx);
                cx.notify();
            });
        });
        let cancel_focus = cx.focus_handle().tab_stop(true);
        let queue_focus = cx.focus_handle().tab_stop(true);
        let download_scroll = gpui::ScrollHandle::new();
        let download_controls = [
            (
                downloads::Control::Active,
                downloads::ScrollControl::new(cancel_focus.clone(), download_scroll.clone()),
            ),
            (
                downloads::Control::Queue,
                downloads::ScrollControl::new(queue_focus.clone(), download_scroll.clone()),
            ),
        ]
        .into();
        let library = cx.new(|cx| library::FileTable::new(false, cx));
        let history = cx.new(|cx| library::FileTable::new(true, cx));
        let library_events = cx.subscribe(&library, |app, _, _: &library::GoToChannels, cx| {
            app.navigate(Screen::Channels, cx)
        });
        let history_events = cx.subscribe(&history, |app, _, _: &library::GoToChannels, cx| {
            app.navigate(Screen::Channels, cx)
        });
        let recommendations =
            cx.new(|cx| recommendations::RecommendationsView::new(connection.commands.clone(), cx));
        let catalog = cx.new(|cx| channels::CatalogView::new(connection.commands.clone(), cx));
        let preferences = cx.new(|cx| settings::SettingsView::new(connection.commands.clone(), cx));
        let notification_test_events = cx.subscribe(
            &preferences,
            |app, _, _: &settings::TestNotification, cx| app.test_notification(cx),
        );
        let catalog_events =
            cx.subscribe(&catalog, |app, _, _: &channels::OpenRecommendations, cx| {
                app.show_recommendations = true;
                cx.notify();
            });
        let recommendation_events = cx.subscribe(
            &recommendations,
            |app, _, event: &recommendations::GoToSource, cx| {
                app.show_recommendations = false;
                app.navigate(Screen::Channels, cx);
                app.catalog
                    .update(cx, |catalog, cx| catalog.select(Some(event.0), cx));
            },
        );
        Self {
            screen: Screen::default(),
            focus_handle: cx.focus_handle(),
            sidebar_toggle_focus: cx.focus_handle().tab_stop(true),
            cancel_focus,
            queue_focus,
            download_scroll,
            download_controls,
            focus_download: false,
            sidebar_open: true,
            commands: connection.commands.clone(),
            info: None,
            records: Vec::new(),
            transfer: TransferState::default(),
            metrics: None,
            loading: true,
            notice: None,
            authentication: AuthState {
                busy: true,
                ..Default::default()
            },
            login: cx.new(|cx| login::LoginForm::new(connection.commands.clone(), cx)),
            preferences,
            catalog,
            library,
            history,
            show_history: false,
            recommendations,
            show_recommendations: false,
            _recommendation_events: recommendation_events,
            _catalog_events: catalog_events,
            notifications: notifications::Notifications::default(),
            notification_timer: None,
            notification_window: None,
            notification_available: false,
            _notification_test_events: notification_test_events,
            _library_events: library_events,
            _history_events: history_events,
            _events: task,
        }
    }

    fn apply_event(&mut self, event: CoreEvent, cx: &mut Context<Self>) {
        match event {
            CoreEvent::RecommendationBatch(batch) => self.queue_notification(batch, cx),
            CoreEvent::RecommendationActionFinished => {
                self.recommendations
                    .update(cx, |view, cx| view.finish_command(cx));
            }
            CoreEvent::Recommendations(state) => {
                self.notification_account(state.account, cx);
                self.recommendations
                    .update(cx, |view, cx| view.set_state(state, cx));
            }
            CoreEvent::Ready(info) => {
                self.preferences.update(cx, |view, cx| {
                    view.initialize(info.settings.clone(), info.data_directory.clone(), cx)
                });
                self.catalog.update(cx, |view, cx| {
                    view.set_download_directory(info.settings.download_directory.clone(), cx)
                });
                self.info = Some(info);
            }
            CoreEvent::SettingsSaved(result) => {
                if let Ok(settings) = &result {
                    if !settings.system_notifications {
                        self.clear_notifications(cx);
                    }
                    if let Some(info) = &mut self.info {
                        info.settings = settings.clone();
                    }
                    self.catalog.update(cx, |view, cx| {
                        view.set_download_directory(settings.download_directory.clone(), cx)
                    });
                }
                self.preferences
                    .update(cx, |view, cx| view.complete(result, cx));
            }
            CoreEvent::SourceSaved(result) => {
                self.catalog
                    .update(cx, |catalog, cx| catalog.complete_source(result, cx));
            }
            CoreEvent::HistoryLoaded(records) => {
                self.records = records;
                self.sync_download_controls(cx);
                self.loading = false;
                self.notice = None;
                self.sync_file_tables(cx);
            }
            CoreEvent::Authentication(state) => {
                if state.busy || state.step != AuthStep::Authorized {
                    self.notification_account(None, cx);
                }
                if state.step == AuthStep::Authorized
                    && self.authentication.step != AuthStep::Authorized
                {
                    self.navigate(Screen::Channels, cx);
                } else if state.step != AuthStep::Authorized {
                    self.catalog.update(cx, |catalog, cx| {
                        catalog.set_state(CatalogState::default(), cx)
                    });
                    self.recommendations
                        .update(cx, |view, cx| view.set_state(Default::default(), cx));
                }
                self.login
                    .update(cx, |form, cx| form.set_state(state.clone(), cx));
                self.authentication = state;
            }
            CoreEvent::Catalog(state) => {
                self.notification_account(state.sources.account, cx);
                self.recommendations
                    .update(cx, |view, cx| view.set_catalog(&state, cx));
                self.catalog
                    .update(cx, |catalog, cx| catalog.set_state(state, cx));
            }
            CoreEvent::DownloadState(state) => {
                if state.active != self.transfer.active || state.cancelling {
                    self.metrics = None;
                }
                let first_job = self.transfer.active.is_none()
                    && self.transfer.queued.is_empty()
                    && (state.active.is_some() || !state.queued.is_empty());
                let new_error = state.error.is_some()
                    && (state.error != self.transfer.error
                        || state.error_file != self.transfer.error_file);
                if first_job || new_error {
                    self.show_history = false;
                    self.navigate(Screen::Downloads, cx);
                    self.focus_download = first_job;
                }
                self.catalog
                    .update(cx, |catalog, cx| catalog.set_transfer(state.clone(), cx));
                self.transfer = state;
                self.recommendations.update(cx, |view, cx| {
                    view.set_downloads(&self.records, &self.transfer, cx)
                });
                self.sync_download_controls(cx);
            }
            CoreEvent::DownloadUpdated(record) => {
                let completed = record.status == DownloadStatus::Completed;
                let terminal = matches!(
                    record.status,
                    DownloadStatus::Completed | DownloadStatus::Failed
                );
                if record.status != DownloadStatus::Downloading
                    && self.metrics.is_some_and(|(key, _)| key == record.key)
                {
                    self.metrics = None;
                }
                if let Some(existing) = self.records.iter_mut().find(|existing| {
                    existing.key == record.key && existing.local_path == record.local_path
                }) {
                    *existing = record;
                } else {
                    self.records.insert(0, record);
                }
                if completed {
                    self.sync_download_controls(cx);
                }
                if terminal {
                    self.sync_file_tables(cx);
                }
            }
            CoreEvent::DownloadMetrics { key, metrics } => {
                if self.transfer.active == Some(key)
                    && !self.transfer.cancelling
                    && self.records.iter().any(|record| {
                        record.key == key && record.status == DownloadStatus::Downloading
                    })
                {
                    self.metrics
                        .get_or_insert((key, metrics::DisplayMetrics::default()))
                        .1
                        .update(metrics, std::time::Instant::now());
                }
            }
            CoreEvent::Failed(failure) => {
                self.loading = false;
                if matches!(
                    failure,
                    ServiceFailure::Initialization | ServiceFailure::AlreadyRunning
                ) {
                    self.authentication = AuthState {
                        error: Some(if failure == ServiceFailure::AlreadyRunning {
                            tgbooks_core::AuthError::AlreadyRunning
                        } else {
                            tgbooks_core::AuthError::SecureStorage
                        }),
                        ..Default::default()
                    };
                    self.login.update(cx, |form, cx| {
                        form.set_state(self.authentication.clone(), cx)
                    });
                }
                self.notice = Some(match failure {
                    ServiceFailure::Initialization => {
                        "Не удалось открыть локальные данные. Проверьте права доступа к директории приложения."
                    }
                    ServiceFailure::AlreadyRunning => {
                        "Этот каталог данных уже используется. Закройте другой экземпляр Telegram Books и перезапустите приложение."
                    }
                    ServiceFailure::Storage => {
                        "Не удалось прочитать или обновить историю. Проверьте доступ к SQLite и повторите обновление."
                    }
                });
                self.sync_file_tables(cx);
            }
        }
        self.sync_recommendation_count(cx);
    }

    fn sync_recommendation_count(&self, cx: &mut Context<Self>) {
        let unread = self.recommendations.read(cx).unread_count();
        self.catalog
            .update(cx, |view, cx| view.set_recommendation_count(unread, cx));
    }

    fn sync_download_controls(&mut self, cx: &mut Context<Self>) {
        use downloads::{Control, ScrollControl};
        let mut wanted = std::collections::HashSet::from([Control::Active, Control::Queue]);
        wanted.extend(
            self.transfer
                .queued
                .iter()
                .map(|job| Control::Waiting(job.book.key)),
        );
        self.download_controls.retain(|id, _| wanted.contains(id));
        for id in wanted {
            self.download_controls.entry(id).or_insert_with(|| {
                ScrollControl::new(
                    cx.focus_handle().tab_stop(true),
                    self.download_scroll.clone(),
                )
            });
        }
    }

    fn sync_file_tables(&mut self, cx: &mut Context<Self>) {
        self.recommendations.update(cx, |view, cx| {
            view.set_downloads(&self.records, &self.transfer, cx)
        });
        self.library.update(cx, |view, cx| {
            view.set_records(&self.records, self.loading, cx)
        });
        self.history.update(cx, |view, cx| {
            view.set_records(&self.records, self.loading, cx)
        });
        self.catalog
            .update(cx, |view, cx| view.set_history(&self.records, cx));
        self.sync_recommendation_count(cx);
    }

    fn navigate(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        if screen != Screen::Downloads {
            self.focus_download = false;
        }
        cx.notify();
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        // When a focused navigation row disappears, focus must remain reachable.
        self.sidebar_toggle_focus.focus(window, cx);
        cx.notify();
    }

    fn sidebar_toggle(&self, cx: &Context<Self>) -> AnyElement {
        icon_button(
            "toggle-sidebar",
            "Показать или скрыть боковую панель",
            Icon::Sidebar,
        )
        .track_focus(&self.sidebar_toggle_focus)
        .aria_expanded(self.sidebar_open)
        .aria_keyshortcuts(platform::shortcut("Command+B", "Control+B"))
        .on_click(cx.listener(|view, _, window, cx| view.toggle_sidebar(window, cx)))
        .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
            if is_activation_key(event) {
                view.toggle_sidebar(window, cx);
                cx.stop_propagation();
            }
        }))
        .into_any_element()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Channels {
            if self.show_recommendations {
                self.recommendations.update(cx, |view, cx| view.refresh(cx));
            } else {
                self.catalog.update(cx, |catalog, cx| catalog.refresh(cx));
            }
            cx.notify();
            return;
        }
        if !self.loading && !self.commands.is_closed() {
            match self.commands.try_send(CoreCommand::RefreshHistory) {
                Ok(()) => {
                    self.loading = true;
                    self.notice = None;
                }
                Err(_) => self.notice = Some("Не удалось отправить команду. Повторите позже."),
            }
            cx.notify();
        }
    }

    fn cancel_download(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.transfer.active
            && !self.transfer.cancelling
        {
            if self
                .commands
                .try_send(CoreCommand::CancelDownload(key))
                .is_ok()
            {
                self.transfer.cancelling = true;
            } else {
                self.notice = Some("Не удалось отменить загрузку. Повторите действие.");
            }
            cx.notify();
        }
    }

    fn cancel_queued(
        &mut self,
        key: tgbooks_core::DownloadKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .commands
            .try_send(CoreCommand::CancelDownload(key))
            .is_err()
        {
            self.notice = Some("Не удалось убрать задание из очереди. Повторите действие.");
        }
        // The row's button will disappear after acknowledgement.
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn toggle_queue(&mut self, cx: &mut Context<Self>) {
        if self
            .commands
            .try_send(CoreCommand::SetQueuePaused(!self.transfer.paused))
            .is_err()
        {
            self.notice = Some("Не удалось изменить состояние очереди. Повторите действие.");
        }
        cx.notify();
    }

    fn refresh_control(&self, cx: &Context<Self>) -> AnyElement {
        if self.screen == Screen::Settings {
            return div().into_any_element();
        }
        let loading = if self.screen == Screen::Channels {
            self.catalog.read(cx).busy()
        } else {
            self.loading
        };
        let transferring = self.screen == Screen::Channels && self.transfer.active.is_some();
        if loading || transferring || self.commands.is_closed() {
            // This GPUI revision has no public disabled-button accessibility API.
            // Use non-interactive status text rather than a misleading fake button.
            return div()
                .min_h(px(34.))
                .py_1()
                .text_sm()
                .text_color(Theme::dark().muted)
                .child(if self.commands.is_closed() {
                    "Сервис остановлен"
                } else if transferring {
                    "Идёт загрузка…"
                } else {
                    "Обновление…"
                })
                .into_any_element();
        }
        if self.screen == Screen::Channels && self.authentication.step != AuthStep::Authorized {
            return div().into_any_element();
        }
        button(
            "refresh",
            if self.screen == Screen::Channels {
                "Обновить каналы"
            } else {
                "Обновить"
            },
        )
        .text_sm()
        .on_click(cx.listener(|view, _, _, cx| view.refresh(cx)))
        .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
            if is_activation_key(event) {
                view.refresh(cx);
                cx.stop_propagation();
            }
        }))
        .into_any_element()
    }

    fn recommendation_controls(&self, cx: &Context<Self>) -> Div {
        div().flex().flex_wrap().items_center().gap_2().child(
            button("back-to-channels", "К каналам")
                .on_click(cx.listener(|view, _, _, cx| {
                    view.show_recommendations = false;
                    cx.notify();
                }))
                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                    if is_activation_key(event) {
                        view.show_recommendations = false;
                        cx.stop_propagation();
                        cx.notify();
                    }
                })),
        )
    }

    fn sidebar(&self, compact: bool, cx: &Context<Self>) -> Div {
        let theme = Theme::dark();
        div()
            .flex()
            .flex_col()
            .w(px(if compact { 208. } else { SIDEBAR_WIDTH }))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_5()
                    .h(px(64.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::Library.element())
                    .child("Telegram Books"),
            )
            .child(
                div().flex().flex_col().px_2().gap_1().children(
                    Screen::ALL
                        .into_iter()
                        .enumerate()
                        .filter(|(_, screen)| {
                            matches!(
                                screen,
                                Screen::Channels | Screen::Downloads | Screen::Library
                            )
                        })
                        .map(|(index, screen)| {
                            let selected = self.screen == screen;
                            navigation_button(("navigation", index), screen.label(), screen.icon())
                                .aria_selected(selected)
                                .when(
                                    screen == Screen::Channels
                                        && self.recommendations.read(cx).unread_count() > 0,
                                    |row| {
                                        let unread = self.recommendations.read(cx).unread_count();
                                        row.aria_label(format!("Каналы, новых книг: {unread}"))
                                            .child(div().flex_1())
                                            .child(div().text_sm().px_2().child(unread.to_string()))
                                    },
                                )
                                .when(selected, |style| {
                                    style
                                        .bg(theme.selected)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                })
                                .when(
                                    screen == Screen::Downloads
                                        && (self.transfer.active.is_some()
                                            || !self.transfer.queued.is_empty()),
                                    |row| {
                                        row.child(div().flex_1()).child(
                                            div()
                                                .px_2()
                                                .rounded_sm()
                                                .bg(theme.selected)
                                                .text_sm()
                                                .child(
                                                    (self.transfer.queued.len()
                                                        + usize::from(
                                                            self.transfer.active.is_some(),
                                                        ))
                                                    .to_string(),
                                                ),
                                        )
                                    },
                                )
                                .on_click(
                                    cx.listener(move |view, _, _, cx| view.navigate(screen, cx)),
                                )
                                .on_key_down(cx.listener(
                                    move |view, event: &KeyDownEvent, _, cx| {
                                        if is_activation_key(event) {
                                            view.navigate(screen, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                        }),
                ),
            )
            .child(div().flex_1())
            .child(
                div().px_2().pb_3().child(
                    navigation_button("nav-settings", "Настройки", Icon::Settings)
                        .when(self.screen == Screen::Settings, |row| {
                            row.bg(theme.selected)
                        })
                        .on_click(cx.listener(|app, _, _, cx| app.navigate(Screen::Settings, cx)))
                        .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) {
                                app.navigate(Screen::Settings, cx);
                                cx.stop_propagation();
                            }
                        })),
                ),
            )
            .child(
                icon_button("nav-account", "Telegram-аккаунт", Icon::Account)
                    .w_full()
                    .h_auto()
                    .px_4()
                    .py_3()
                    .border_color(theme.sidebar)
                    .rounded_none()
                    .justify_start()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .child(div().truncate().child(
                                self.authentication.account.as_ref().map_or_else(
                                    || {
                                        if self.authentication.step == AuthStep::Authorized {
                                            "Личный аккаунт".into()
                                        } else {
                                            "Подключить Telegram".into()
                                        }
                                    },
                                    tgbooks_core::AccountProfile::label,
                                ),
                            ))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(theme.muted)
                                    .child(if self.authentication.busy {
                                        "Проверяем Telegram…"
                                    } else if self.authentication.step == AuthStep::Authorized {
                                        "Telegram подключён"
                                    } else {
                                        "Telegram не подключён"
                                    }),
                            ),
                    )
                    .on_click(cx.listener(|app, _, _, cx| app.navigate(Screen::Login, cx)))
                    .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) {
                            app.navigate(Screen::Login, cx);
                            cx.stop_propagation();
                        }
                    })),
            )
    }

    fn content(&self, cx: &Context<Self>) -> Div {
        match self.screen {
            Screen::Login => div().h_full().min_h_0().child(self.login.clone()),
            Screen::Channels if self.show_recommendations => {
                div().h_full().min_h_0().child(self.recommendations.clone())
            }
            Screen::Channels => div().h_full().min_h_0().child(self.catalog.clone()),
            Screen::Downloads if self.show_history => {
                div().h_full().min_h_0().child(self.history.clone())
            }
            Screen::Downloads => downloads::render(self, cx),
            Screen::Library => div().h_full().min_h_0().child(self.library.clone()),
            Screen::Settings => div().h_full().min_h_0().child(self.preferences.clone()),
        }
    }
}

fn is_activation_key(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
}

impl Render for BookApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.notification_window = window.window_handle().downcast::<Self>();
        for control in self.download_controls.values_mut() {
            control.observe(window, cx);
        }
        if self.screen == Screen::Downloads {
            if self.focus_download && self.transfer.active.is_some() && !self.transfer.cancelling {
                self.cancel_focus.focus(window, cx);
            } else if (self.transfer.active.is_none() || self.transfer.cancelling)
                && self.cancel_focus.is_focused(window)
            {
                self.focus_handle.focus(window, cx);
            }
            if self.transfer.queued.is_empty() && self.queue_focus.is_focused(window) {
                self.focus_handle.focus(window, cx);
            }
        }
        if self.transfer.active.is_some() || self.transfer.queued.is_empty() {
            self.focus_download = false;
        }
        let theme = Theme::dark();
        let short = window.viewport_size().height < px(620.);
        div()
            .id("application")
            .role(Role::Application)
            .aria_label("Telegram Books")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|view, _: &FocusNext, window, cx| {
                window.focus_next(cx);
                if view.screen == Screen::Settings {
                    view.preferences
                        .update(cx, |view, cx| view.reveal_focus(window, cx));
                }
            }))
            .on_action(cx.listener(|view, _: &FocusPrevious, window, cx| {
                window.focus_prev(cx);
                if view.screen == Screen::Settings {
                    view.preferences
                        .update(cx, |view, cx| view.reveal_focus(window, cx));
                }
            }))
            .on_action(cx.listener(|view, _: &Login, _, cx| view.navigate(Screen::Login, cx)))
            .on_action(cx.listener(|view, _: &Channels, _, cx| view.navigate(Screen::Channels, cx)))
            .on_action(
                cx.listener(|view, _: &Downloads, _, cx| view.navigate(Screen::Downloads, cx)),
            )
            .on_action(cx.listener(|view, _: &Library, _, cx| view.navigate(Screen::Library, cx)))
            .on_action(cx.listener(|view, _: &Settings, _, cx| view.navigate(Screen::Settings, cx)))
            .on_action(cx.listener(|view, _: &Refresh, _, cx| view.refresh(cx)))
            .on_action(
                cx.listener(|view, _: &ToggleSidebar, window, cx| view.toggle_sidebar(window, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas)
            .text_color(theme.text)
            .font_family(platform::font_family())
            .text_size(px(14.))
            .child(chrome::toolbar(self.sidebar_toggle(cx)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_open, |body| {
                        body.child(self.sidebar(window.viewport_size().width < px(1100.), cx))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .p(px(if short { 16. } else { 24. }))
                            .gap(px(if short { 12. } else { 16. }))
                            .when(
                                self.screen != Screen::Channels || self.show_recommendations,
                                |body| {
                                    body.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .flex_shrink_0()
                                            .gap_4()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_col()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .id("screen-title")
                                                            .role(Role::Heading)
                                                            .aria_level(1)
                                                            .aria_label(
                                                                if self.screen == Screen::Channels
                                                                    && self.show_recommendations
                                                                {
                                                                    "Новые книги"
                                                                } else {
                                                                    self.screen.label()
                                                                },
                                                            )
                                                            .text_xl()
                                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                                            .child(
                                                                if self.screen == Screen::Channels
                                                                    && self.show_recommendations
                                                                {
                                                                    "Новые книги"
                                                                } else {
                                                                    self.screen.label()
                                                                },
                                                            ),
                                                    )
                                                    .when(!short, |heading| {
                                                        heading.child(
                                                    div().text_sm().text_color(theme.muted).child(
                                                        match self.screen {
                                                            Screen::Channels
                                                                if self.show_recommendations =>
                                                            {
                                                                format!(
                                                                    "Непросмотренных находок: {}",
                                                                    self.recommendations
                                                                        .read(cx)
                                                                        .unread_count()
                                                                )
                                                            }
                                                            Screen::Downloads => {
                                                                "Скачивание по одному файлу".into()
                                                            }
                                                            Screen::Library => format!(
                                                                "Файлов на этом устройстве: {}",
                                                                self.records
                                                                    .iter()
                                                                    .filter(|record| {
                                                                        record.status
                                                                    == DownloadStatus::Completed
                                                                    })
                                                                    .count()
                                                            ),
                                                            _ => self.screen.description().into(),
                                                        },
                                                    ),
                                                )
                                                    }),
                                            )
                                            .child(if self.screen == Screen::Channels {
                                                self.recommendation_controls(cx).into_any_element()
                                            } else if self.screen == Screen::Downloads
                                                && !self.show_history
                                            {
                                                downloads::queue_control(self, cx)
                                            } else {
                                                self.refresh_control(cx)
                                            }),
                                    )
                                },
                            )
                            .when(self.screen == Screen::Downloads, |body| {
                                body.child(downloads::tabs(self, cx))
                            })
                            .child(
                                div()
                                    .id("screen-content")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_hidden()
                                    .when(
                                        self.screen == Screen::Downloads && !self.show_history,
                                        |content| {
                                            content
                                                .overflow_y_scroll()
                                                .track_scroll(&self.download_scroll)
                                        },
                                    )
                                    .child(self.content(cx)),
                            )
                            .child(
                                div()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .pt_2()
                                    .text_size(px(11.))
                                    .text_color(if self.notice.is_some() {
                                        theme.error
                                    } else {
                                        theme.muted
                                    })
                                    .child(SharedString::from(self.notice.unwrap_or(
                                        if self.loading {
                                            "Обновляем локальные данные…"
                                        } else {
                                            "Все данные хранятся на этом устройстве"
                                        },
                                    ))),
                            ),
                    ),
            )
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use gpui::TestAppContext;
    use tgbooks_core::BookFormat;

    use super::*;

    #[gpui::test]
    fn navigation_and_history_commands_use_gpui_entities(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        app.update(cx, |app, cx| {
            app.navigate(Screen::Library, cx);
            app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
            app.refresh(cx);
        });
        app.read_with(cx, |app, _| {
            assert_eq!(app.screen, Screen::Library);
            assert!(app.loading);
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::RefreshHistory)
        ));
    }

    #[gpui::test]
    fn channel_actions_align_with_the_title_without_a_separate_page_header(
        cx: &mut TestAppContext,
    ) {
        for (width, height) in [(1280., 800.), (760., 520.)] {
            for selected in [true, false] {
                let (ui, backend) = tgbooks_core::connect();
                let window = cx.open_window(size(px(width), px(height)), |_, cx| {
                    let mut app = BookApp::new(ui, cx);
                    app.apply_event(
                        CoreEvent::Authentication(AuthState {
                            step: AuthStep::Authorized,
                            ..Default::default()
                        }),
                        cx,
                    );
                    app.apply_event(
                        CoreEvent::Catalog(CatalogState {
                            authenticated: true,
                            channels: vec![tgbooks_core::ChannelSummary {
                                id: -1,
                                title: "Physics.Math.Code".into(),
                                username: Some("physics_lib".into()),
                                protected: false,
                            }],
                            selected: selected.then_some(-1),
                            sources: tgbooks_core::SavedSources {
                                account: Some(1),
                                loaded: true,
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                        cx,
                    );
                    app
                });
                let Ok(app) = window.root(cx) else {
                    panic!("window missing");
                };
                let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
                visual.update(|window, _| window.activate_window());
                visual.run_until_parked();
                let Some(title) = visual.debug_bounds(if selected {
                    "channel-title"
                } else {
                    "channel-overview-title"
                }) else {
                    panic!("title missing");
                };
                let Some(new_books) = visual.debug_bounds("catalog-new-books") else {
                    panic!("new books action missing");
                };
                let Some(refresh) = visual.debug_bounds("catalog-refresh") else {
                    panic!("refresh action missing");
                };
                assert!((new_books.center().y - title.center().y).abs() <= px(1.));
                assert!((refresh.center().y - title.center().y).abs() <= px(1.));
                assert!(
                    title.top() < px(90.),
                    "a separate header still pushes content down: {title:?}"
                );
                assert!(new_books.right() <= refresh.left() && refresh.right() <= px(width));
                visual.simulate_click(refresh.center(), gpui::Modifiers::default());
                assert!(matches!(
                    backend.commands.try_recv(),
                    Ok(CoreCommand::RefreshChannels)
                ));
                visual.run_until_parked();
                let Some(new_books) = visual.debug_bounds("catalog-new-books") else {
                    panic!("new books action disappeared");
                };
                visual.simulate_click(new_books.center(), gpui::Modifiers::default());
                visual.run_until_parked();
                app.read_with(&visual.cx, |app, _| assert!(app.show_recommendations));
                assert!(backend.commands.try_recv().is_err());
            }
        }
    }

    #[gpui::test]
    fn authorized_login_opens_channels_and_refresh_targets_the_catalog(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        app.update(cx, |app, cx| {
            app.apply_event(
                CoreEvent::Authentication(AuthState {
                    step: AuthStep::Authorized,
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(
                CoreEvent::Catalog(CatalogState {
                    authenticated: true,
                    ..Default::default()
                }),
                cx,
            );
            app.refresh(cx);
            app.refresh(cx);
            assert_eq!(app.screen, Screen::Channels);
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::RefreshChannels)
        ));
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn download_progress_updates_one_record_and_keyboard_can_cancel(cx: &mut TestAppContext) {
        use tgbooks_core::{DownloadKey, DownloadProgress, DownloadStatus};

        let (ui, backend) = tgbooks_core::connect();
        let (app, cx) = cx.add_window_view(|window, cx| {
            bind_keys(cx);
            let app = BookApp::new(ui, cx);
            app.focus_handle.focus(window, cx);
            app
        });
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 7,
            document_id: 8,
        };
        app.update(&mut cx.cx, |app, cx| {
            app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(key),
                    ..Default::default()
                }),
                cx,
            );
            for bytes in [0, 40] {
                let Ok(progress) = DownloadProgress::new(bytes, Some(100)) else {
                    panic!("invalid test progress")
                };
                app.apply_event(
                    CoreEvent::DownloadUpdated(DownloadRecord {
                        key,
                        file_name: "book.epub".into(),
                        source_title: None,
                        added_at: 0,
                        local_path: "books/book.epub".into(),
                        status: DownloadStatus::Downloading,
                        progress,
                    }),
                    cx,
                );
            }
            cx.notify();
        });
        cx.run_until_parked();
        app.read_with(&cx.cx, |app, _| {
            assert_eq!(app.screen, Screen::Downloads);
            assert_eq!(app.records.len(), 1);
            assert_eq!(app.records[0].progress.percentage(), Some(40));
        });
        cx.simulate_keystrokes("enter");
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::CancelDownload(actual)) if actual == key)
        );
        app.read_with(&cx.cx, |app, _| assert!(app.transfer.cancelling));
        cx.run_until_parked();
        cx.update(|window, cx| assert!(app.read(cx).focus_handle.is_focused(window)));
    }

    #[gpui::test]
    fn waiting_queue_supports_keyboard_pause_and_cancel_without_navigation_on_next_file(
        cx: &mut TestAppContext,
    ) {
        use tgbooks_core::{BookAttachment, DownloadKey, QueuedDownload};
        let (ui, backend) = tgbooks_core::connect();
        let (app, cx) = cx.add_window_view(|_, cx| {
            bind_keys(cx);
            BookApp::new(ui, cx)
        });
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 7,
            document_id: 8,
        };
        let job = QueuedDownload {
            interrupted: true,
            redownload: false,
            book: BookAttachment {
                key,
                file_name: "book.epub".into(),
                size_bytes: Some(10),
                format: BookFormat::Epub,
            },
            directory: std::env::temp_dir(),
        };
        app.update(&mut cx.cx, |app, cx| {
            app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    queued: vec![job.clone()],
                    paused: true,
                    restored: true,
                    ..Default::default()
                }),
                cx,
            );
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.activate_window();
            app.read(cx).queue_focus.clone().focus(window, cx);
        });
        cx.simulate_keystrokes("enter");
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SetQueuePaused(false))
        ));
        // Queue/history tabs precede the waiting row's removal button.
        cx.simulate_keystrokes("tab tab tab enter");
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::CancelDownload(actual)) if actual == key)
        );
        cx.update(|window, cx| assert!(app.read(cx).focus_handle.is_focused(window)));
        app.update(&mut cx.cx, |app, cx| {
            app.navigate(Screen::Channels, cx);
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(key),
                    ..Default::default()
                }),
                cx,
            );
            assert_eq!(
                app.screen,
                Screen::Channels,
                "starting the next job stole navigation"
            );
            assert!(!app.focus_download);
            app.apply_event(CoreEvent::DownloadState(TransferState::default()), cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn focused_waiting_controls_scroll_into_view_in_a_small_window(cx: &mut TestAppContext) {
        use tgbooks_core::{BookAttachment, DownloadKey, QueuedDownload};
        let (ui, _backend) = tgbooks_core::connect();
        let window = cx.open_window(size(px(760.), px(520.)), |_, cx| BookApp::new(ui, cx));
        let Ok(app) = window.root(cx) else {
            panic!("queue window missing")
        };
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let jobs: Vec<_> = (0..20)
            .map(|index| QueuedDownload {
                interrupted: false,
                redownload: false,
                book: BookAttachment {
                    key: DownloadKey {
                        channel_id: -1000000000001,
                        message_id: index,
                        document_id: i64::from(index),
                    },
                    file_name: format!("Очень длинное название книги 世界 {index}.epub"),
                    size_bytes: Some(10),
                    format: BookFormat::Epub,
                },
                directory: std::env::temp_dir(),
            })
            .collect();
        app.update(&mut visual.cx, |app, cx| {
            app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    queued: jobs.clone(),
                    paused: true,
                    ..Default::default()
                }),
                cx,
            );
        });
        visual.run_until_parked();
        for (index, selector) in [(19, "cancel-queued-19"), (0, "cancel-queued-0")] {
            let focus = app.read_with(&visual.cx, |app, _| {
                app.download_controls[&downloads::Control::Waiting(jobs[index].book.key)]
                    .focus
                    .clone()
            });
            visual.update(|window, cx| {
                window.activate_window();
                focus.focus(window, cx);
            });
            visual.run_until_parked();
            visual.update(|window, cx| {
                // GPUI's test platform has no native display frame loop.
                window.simulate_next_frame(cx);
            });
            visual.run_until_parked();
            let Some(bounds) = visual.debug_bounds(selector) else {
                panic!("queued button was not drawn")
            };
            app.read_with(&visual.cx, |app, _| {
                let viewport = app.download_scroll.bounds();
                assert!(
                    bounds.top() >= viewport.top() - px(1.),
                    "{selector}: {bounds:?}, {viewport:?}, offset {:?}",
                    app.download_scroll.offset()
                );
                assert!(
                    bounds.bottom() <= viewport.bottom() + px(1.),
                    "{selector}: {bounds:?}, {viewport:?}, offset {:?}",
                    app.download_scroll.offset()
                );
            });
        }
    }

    #[gpui::test]
    fn live_metrics_are_scoped_to_one_attempt_and_never_restored_from_history(
        cx: &mut TestAppContext,
    ) {
        use tgbooks_core::{DownloadKey, DownloadProgress, TransferMetrics};
        let (ui, _backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 1,
            document_id: 2,
        };
        let another = DownloadKey {
            message_id: 2,
            ..key
        };
        let metrics = TransferMetrics {
            bytes_per_second: Some(20),
            remaining_seconds: Some(3),
        };
        let Ok(progress) = DownloadProgress::new(40, Some(100)) else {
            panic!("invalid test progress")
        };
        let mut record = DownloadRecord {
            key,
            file_name: "books.zip".into(),
            source_title: None,
            added_at: 0,
            local_path: "books.zip".into(),
            status: DownloadStatus::Downloading,
            progress,
        };
        app.update(cx, |app, cx| {
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(key),
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(CoreEvent::DownloadUpdated(record.clone()), cx);
            app.apply_event(
                CoreEvent::DownloadMetrics {
                    key,
                    metrics: TransferMetrics {
                        bytes_per_second: Some(0),
                        remaining_seconds: None,
                    },
                },
                cx,
            );
            assert_eq!(
                app.metrics
                    .and_then(|(_, display)| display.values.bytes_per_second),
                None
            );
            app.apply_event(CoreEvent::DownloadMetrics { key, metrics }, cx);
            assert_eq!(
                app.metrics.map(|(key, display)| (key, display.values)),
                Some((key, metrics))
            );
            app.apply_event(
                CoreEvent::DownloadMetrics {
                    key: another,
                    metrics: TransferMetrics::default(),
                },
                cx,
            );
            assert_eq!(
                app.metrics.map(|(key, display)| (key, display.values)),
                Some((key, metrics))
            );
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(key),
                    cancelling: true,
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(CoreEvent::DownloadMetrics { key, metrics }, cx);
            assert_eq!(app.metrics, None);
            // Another attempt must measure again even when its key is unchanged.
            app.apply_event(CoreEvent::DownloadState(TransferState::default()), cx);
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(key),
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(CoreEvent::HistoryLoaded(vec![record.clone()]), cx);
            assert_eq!(app.metrics, None);
            app.apply_event(CoreEvent::DownloadMetrics { key, metrics }, cx);
            record.status = DownloadStatus::Completed;
            let Ok(progress) = DownloadProgress::new(100, Some(100)) else {
                panic!("invalid test progress")
            };
            record.progress = progress;
            app.apply_event(CoreEvent::DownloadUpdated(record), cx);
            app.apply_event(CoreEvent::DownloadMetrics { key, metrics }, cx);
            assert_eq!(
                app.metrics, None,
                "late metrics revived a completed transfer"
            );
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(another),
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(CoreEvent::DownloadMetrics { key, metrics }, cx);
            assert_eq!(app.metrics, None);
        });
    }

    #[gpui::test]
    fn metric_column_stays_right_aligned_and_stable_in_a_compact_window(cx: &mut TestAppContext) {
        use tgbooks_core::{DownloadKey, DownloadProgress, TransferMetrics};
        let (ui, _backend) = tgbooks_core::connect();
        let window = cx.open_window(size(px(760.), px(520.)), |_, cx| BookApp::new(ui, cx));
        let Ok(app) = window.root(cx) else {
            panic!("download window missing")
        };
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 1,
            document_id: 2,
        };
        let Ok(progress) = DownloadProgress::new(100_000_000, Some(1_400_000_000)) else {
            panic!("invalid test progress")
        };
        let record = DownloadRecord {
            key,
            file_name: "Полное собрание книг с очень длинным названием 世界.zip".into(),
            source_title: None,
            added_at: 0,
            local_path: "books.zip".into(),
            status: DownloadStatus::Downloading,
            progress,
        };
        let metrics = TransferMetrics {
            bytes_per_second: Some(409_600),
            remaining_seconds: Some(9300),
        };
        app.update(&mut visual.cx, |app, cx| {
            app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
            app.apply_event(
                CoreEvent::DownloadState(TransferState {
                    active: Some(key),
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(CoreEvent::DownloadUpdated(record), cx);
            app.apply_event(CoreEvent::DownloadMetrics { key, metrics }, cx);
        });
        visual.run_until_parked();
        let Some(speed) = visual.debug_bounds("download-speed") else {
            panic!("speed missing")
        };
        let Some(eta) = visual.debug_bounds("download-eta") else {
            panic!("ETA missing")
        };
        assert_eq!(speed.right(), eta.right());
        assert!(eta.top() >= speed.bottom());
        app.read_with(&visual.cx, |app, _| {
            // The new active panel has a 16px inset and a 1px border.
            assert!(
                (speed.right() - app.download_scroll.bounds().right() + px(17.)).abs() <= px(1.)
            );
        });
        app.update(&mut visual.cx, |app, cx| {
            // Fast incoming updates must not change the visible sample.
            app.apply_event(
                CoreEvent::DownloadMetrics {
                    key,
                    metrics: TransferMetrics {
                        bytes_per_second: Some(999_999),
                        remaining_seconds: Some(9999),
                    },
                },
                cx,
            );
            assert_eq!(
                app.metrics.map(|(_, display)| display.values),
                Some(metrics)
            );
            // Simulate both presentation periods expiring without a real sleep.
            if let Some((_, display)) = app.metrics.as_mut() {
                display.update(
                    TransferMetrics {
                        bytes_per_second: Some(u64::MAX),
                        remaining_seconds: Some(u64::MAX),
                    },
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                );
            }
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("download-speed"), Some(speed));
        assert_eq!(visual.debug_bounds("download-eta"), Some(eta));
    }

    #[gpui::test]
    fn async_events_update_the_view_without_a_tokio_context(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        assert!(
            backend
                .events
                .try_send(CoreEvent::Ready(AppInfo {
                    settings: tgbooks_core::AppSettings::new("books".into()),
                    data_directory: "data".into(),
                    download_directory: "books".into(),
                }))
                .is_ok()
        );
        assert!(
            backend
                .events
                .try_send(CoreEvent::HistoryLoaded(Vec::new()))
                .is_ok()
        );
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert!(app.info.is_some());
            assert!(!app.loading);
            assert!(app.notice.is_none());
        });
    }

    #[gpui::test]
    fn rendered_navigation_supports_shortcuts_and_tab_activation(cx: &mut TestAppContext) {
        let (ui, _backend) = tgbooks_core::connect();
        let (app, cx) = cx.add_window_view(|window, cx| {
            bind_keys(cx);
            let app = BookApp::new(ui, cx);
            app.focus_handle.focus(window, cx);
            app
        });
        for (keystroke, screen) in [
            (platform::shortcut("cmd-2", "ctrl-2"), Screen::Channels),
            (platform::shortcut("cmd-3", "ctrl-3"), Screen::Downloads),
            (platform::shortcut("cmd-4", "ctrl-4"), Screen::Library),
            (platform::shortcut("cmd-5", "ctrl-5"), Screen::Settings),
        ] {
            cx.simulate_keystrokes(keystroke);
            app.read_with(&cx.cx, |app, _| assert_eq!(app.screen, screen));
        }
        cx.simulate_keystrokes("tab tab enter");
        app.read_with(&cx.cx, |app, _| assert_eq!(app.screen, Screen::Channels));
    }

    #[gpui::test]
    fn sidebar_toggle_keeps_focus_reachable_and_preserves_the_screen(cx: &mut TestAppContext) {
        let (ui, _backend) = tgbooks_core::connect();
        let (app, cx) = cx.add_window_view(|window, cx| {
            bind_keys(cx);
            let app = BookApp::new(ui, cx);
            app.focus_handle.focus(window, cx);
            app
        });
        cx.simulate_keystrokes("tab tab enter");
        app.read_with(&cx.cx, |app, _| assert_eq!(app.screen, Screen::Channels));
        cx.simulate_keystrokes(platform::shortcut("cmd-b", "ctrl-b"));
        app.read_with(&cx.cx, |app, _| {
            assert!(!app.sidebar_open);
            assert_eq!(app.screen, Screen::Channels);
        });
        cx.update(|window, cx| {
            assert!(app.read(cx).sidebar_toggle_focus.is_focused(window));
        });
        cx.simulate_keystrokes("enter");
        app.read_with(&cx.cx, |app, _| assert!(app.sidebar_open));
        cx.simulate_keystrokes("tab enter");
        app.read_with(&cx.cx, |app, _| assert_eq!(app.screen, Screen::Channels));
    }

    #[gpui::test]
    fn native_text_input_tab_and_enter_drive_every_authentication_step(cx: &mut TestAppContext) {
        use tgbooks_core::AuthAction;

        let (ui, backend) = tgbooks_core::connect();
        let (app, cx) = cx.add_window_view(|window, cx| {
            bind_keys(cx);
            let app = BookApp::new(ui, cx);
            app.focus_handle.focus(window, cx);
            app
        });
        app.update(&mut cx.cx, |app, cx| {
            app.apply_event(CoreEvent::Authentication(AuthState::default()), cx);
        });
        cx.run_until_parked();
        cx.simulate_input("1");
        cx.simulate_keystrokes("tab");
        cx.simulate_input(&"0".repeat(32));
        cx.simulate_keystrokes("enter");
        let Ok(CoreCommand::Authenticate(AuthAction::Configure { api_id, api_hash })) =
            backend.commands.try_recv()
        else {
            panic!("credentials were not sent from native fields")
        };
        assert_eq!(api_id, 1);
        assert_eq!(api_hash.expose(), "0".repeat(32));
        for (step, value) in [
            (AuthStep::Phone, "+15551234567"),
            (AuthStep::Code, "12345"),
            (AuthStep::Password, "dummy password"),
        ] {
            app.update(&mut cx.cx, |app, cx| {
                app.apply_event(
                    CoreEvent::Authentication(AuthState {
                        step,
                        ..Default::default()
                    }),
                    cx,
                );
            });
            cx.run_until_parked();
            cx.simulate_input(value);
            cx.simulate_keystrokes("enter");
            let Ok(CoreCommand::Authenticate(action)) = backend.commands.try_recv() else {
                panic!("authentication command was not sent")
            };
            let secret = match action {
                AuthAction::RequestCode(value)
                | AuthAction::SubmitCode(value)
                | AuthAction::SubmitPassword(value) => value,
                _ => panic!("unexpected command"),
            };
            assert_eq!(secret.expose(), value);
        }
    }

    #[gpui::test]
    fn second_instance_error_is_not_replaced_by_a_network_error(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        assert!(
            backend
                .events
                .try_send(CoreEvent::Failed(ServiceFailure::AlreadyRunning))
                .is_ok()
        );
        drop(backend);
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(
                app.authentication.error,
                Some(tgbooks_core::AuthError::AlreadyRunning)
            );
            assert!(!app.authentication.busy);
            assert!(
                app.notice
                    .is_some_and(|notice| notice.contains("другой экземпляр"))
            );
        });
    }

    #[gpui::test]
    fn service_shutdown_does_not_hide_the_initialization_error(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        assert!(
            backend
                .events
                .try_send(CoreEvent::Failed(ServiceFailure::Initialization))
                .is_ok()
        );
        drop(backend);
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert!(!app.loading);
            assert!(app.commands.is_closed());
            assert!(
                app.notice
                    .is_some_and(|notice| notice.contains("локальные данные"))
            );
        });
    }
}
