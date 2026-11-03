mod chrome;
mod components;
mod icons;
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
    AppInfo, BookFormat, CoreCommand, CoreEvent, DownloadRecord, DownloadStatus, ServiceFailure,
    UiConnection,
};

use crate::platform;
use components::{button, empty_state, icon_button, navigation_button, setting};
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

pub fn run(connection: UiConnection, shutdown: impl Future<Output = ()> + 'static) -> Result<()> {
    let window_error = Rc::new(Cell::new(None));
    let error = window_error.clone();
    gpui_platform::application()
        .with_assets(IconAssets)
        .run(move |cx: &mut App| {
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
            let bounds = Bounds::centered(None, size(px(1040.), px(700.)), cx);
            if let Err(failure) =
                cx.open_window(platform::window_options(bounds), move |window, cx| {
                    let view = cx.new(|cx| BookApp::new(connection, cx));
                    view.read(cx).focus_handle.clone().focus(window, cx);
                    view
                })
            {
                error.set(Some(failure));
                cx.quit();
                return;
            }
            cx.activate(true);
            tracing::info!("native window ready");
        });
    match window_error.take() {
        Some(error) => Err(error).context("could not open the native window"),
        None => Ok(()),
    }
}

fn bind_keys(cx: &mut App) {
    use platform::shortcut;

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
    sidebar_open: bool,
    commands: Sender<CoreCommand>,
    info: Option<AppInfo>,
    records: Vec<DownloadRecord>,
    loading: bool,
    notice: Option<&'static str>,
    _events: Task<()>,
}

impl BookApp {
    fn new(connection: UiConnection, cx: &mut Context<Self>) -> Self {
        let events = connection.events;
        let task = cx.spawn(async move |view, cx| {
            while let Ok(event) = events.recv().await {
                if view
                    .update(cx, |view, cx| {
                        view.apply_event(event);
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
                cx.notify();
            });
        });
        Self {
            screen: Screen::default(),
            focus_handle: cx.focus_handle(),
            sidebar_toggle_focus: cx.focus_handle().tab_stop(true),
            sidebar_open: true,
            commands: connection.commands,
            info: None,
            records: Vec::new(),
            loading: true,
            notice: None,
            _events: task,
        }
    }

    fn apply_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Ready(info) => self.info = Some(info),
            CoreEvent::HistoryLoaded(records) => {
                self.records = records;
                self.loading = false;
                self.notice = None;
            }
            CoreEvent::Failed(failure) => {
                self.loading = false;
                self.notice = Some(match failure {
                    ServiceFailure::Initialization => {
                        "Не удалось открыть локальные данные. Проверьте права доступа к директории приложения."
                    }
                    ServiceFailure::Storage => {
                        "Не удалось прочитать историю. Проверьте доступ к SQLite и повторите обновление."
                    }
                });
            }
        }
    }

    fn navigate(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
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

    fn refresh_control(&self, cx: &Context<Self>) -> AnyElement {
        if self.loading || self.commands.is_closed() {
            // This GPUI revision has no public disabled-button accessibility API.
            // Use non-interactive status text rather than a misleading fake button.
            return div()
                .min_h(px(34.))
                .py_1()
                .text_sm()
                .text_color(Theme::dark().muted)
                .child(if self.loading {
                    "Обновление…"
                } else {
                    "Сервис остановлен"
                })
                .into_any_element();
        }
        button("refresh-history", "Обновить")
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

    fn sidebar(&self, cx: &Context<Self>) -> Div {
        let theme = Theme::dark();
        div()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_3()
                    .pt_4()
                    .pb_2()
                    .text_sm()
                    .text_color(theme.muted)
                    .child("Библиотека"),
            )
            .child(div().flex().flex_col().px_2().gap_1().children(
                Screen::ALL.into_iter().enumerate().map(|(index, screen)| {
                    let selected = self.screen == screen;
                    navigation_button(("navigation", index), screen.label(), screen.icon())
                        .aria_selected(selected)
                        .when(selected, |style| {
                            style
                                .bg(theme.selected)
                                .font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .on_click(cx.listener(move |view, _, _, cx| view.navigate(screen, cx)))
                        .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) {
                                view.navigate(screen, cx);
                                cx.stop_propagation();
                            }
                        }))
                }),
            ))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child("Telegram Books")
                    .text_color(theme.muted)
                    .child("Telegram не подключён"),
            )
    }

    fn content(&self) -> Div {
        match self.screen {
            Screen::Login => empty_state(
                "Подключите Telegram",
                "Вход по номеру телефона, коду и 2FA появится в следующей итерации. Пока доступны навигация и локальная история загрузок.",
            ),
            Screen::Channels => empty_state(
                "Каналы появятся после входа",
                "В следующей итерации здесь будут доступны публичные и приватные каналы, участником которых является ваш аккаунт.",
            ),
            Screen::Downloads | Screen::Library => self.downloads(),
            Screen::Settings => {
                let mut settings = div().flex().flex_col();
                if let Some(info) = &self.info {
                    settings = settings
                        .child(setting("Директория данных", info.data_directory.clone()))
                        .child(setting("Директория книг", info.download_directory.clone()));
                }
                settings
                    .child(setting(
                        "Форматы книг",
                        BookFormat::ALL
                            .map(|format| format.extension().to_uppercase())
                            .join(", "),
                    ))
                    .child(setting("Тема", "Тёмная, в стиле Delta"))
                    .child(
                        div()
                            .pt_6()
                            .text_color(Theme::dark().muted)
                            .child("Параметры пока доступны только для просмотра. Выбор директории будет добавлен вместе со скачиванием."),
                    )
            }
        }
    }

    fn downloads(&self) -> Div {
        let records: Vec<_> = self
            .records
            .iter()
            .filter(|record| {
                self.screen != Screen::Library || record.status == DownloadStatus::Completed
            })
            .collect();
        if records.is_empty() {
            return empty_state(
                if self.loading {
                    "Читаем локальную историю…"
                } else if self.screen == Screen::Library {
                    "Библиотека пока пуста"
                } else {
                    "Загрузок пока нет"
                },
                "Здесь отображается реальная история из SQLite. Скачивание из Telegram ещё не подключено; демонстрационные записи не создаются.",
            );
        }
        let theme = Theme::dark();
        div()
            .flex()
            .flex_col()
            .children(records.into_iter().map(|record| {
                let progress = match record.progress.percentage() {
                    Some(percent) => format!("{percent}%"),
                    None => format!("{} байт", record.progress.downloaded_bytes()),
                };
                let status = match record.status {
                    DownloadStatus::Queued => "В очереди",
                    DownloadStatus::Downloading => "Скачивается",
                    DownloadStatus::Completed => "Скачано",
                    DownloadStatus::Failed => "Ошибка",
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(record.file_name.clone())
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted)
                            .child(format!("{status}: {progress}")),
                    )
            }))
    }
}

fn is_activation_key(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
}

impl Render for BookApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        div()
            .id("application")
            .role(Role::Application)
            .aria_label("Telegram Books")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, window, cx| window.focus_prev(cx)))
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
                    .when(self.sidebar_open, |body| body.child(self.sidebar(cx)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .p_6()
                            .gap_6()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
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
                                                    .aria_label(self.screen.label())
                                                    .text_xl()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(self.screen.label()),
                                            )
                                            .child(
                                                div()
                                                    .text_color(theme.muted)
                                                    .child(self.screen.description()),
                                            ),
                                    )
                                    .child(self.refresh_control(cx)),
                            )
                            .child(
                                div()
                                    .id("screen-content")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(self.content()),
                            )
                            .child(
                                div()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .pt_4()
                                    .text_sm()
                                    .text_color(if self.notice.is_some() {
                                        theme.error
                                    } else {
                                        theme.muted
                                    })
                                    .child(SharedString::from(self.notice.unwrap_or(
                                        if self.loading {
                                            "Обновляем локальные данные…"
                                        } else {
                                            "Локальное хранилище готово"
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

    use super::*;

    #[gpui::test]
    fn navigation_and_history_commands_use_gpui_entities(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        app.update(cx, |app, cx| {
            app.navigate(Screen::Library, cx);
            app.apply_event(CoreEvent::HistoryLoaded(Vec::new()));
            app.refresh(cx);
        });
        app.read_with(cx, |app, _| {
            assert_eq!(app.screen, Screen::Library);
            assert!(app.loading);
        });
        assert_eq!(backend.commands.try_recv(), Ok(CoreCommand::RefreshHistory));
    }

    #[gpui::test]
    fn async_events_update_the_view_without_a_tokio_context(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let app = cx.new(|cx| BookApp::new(ui, cx));
        assert!(
            backend
                .events
                .try_send(CoreEvent::Ready(AppInfo {
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
        app.read_with(&cx.cx, |app, _| assert_eq!(app.screen, Screen::Login));
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
        cx.simulate_keystrokes("tab tab tab enter");
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
        app.read_with(&cx.cx, |app, _| assert_eq!(app.screen, Screen::Login));
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
