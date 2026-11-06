//! Editable preferences use typed Core commands, never direct filesystem access.
use async_channel::Sender;
use gpui::{
    Context, EventEmitter, FocusHandle, KeyDownEvent, Role, ScrollHandle, Window, div, prelude::*,
};
use tgbooks_core::{AppSettings, BookFormat, CoreCommand, SettingsError};

use super::{
    components::{button, primary_button, setting, switch},
    icons::Icon,
    is_activation_key,
    theme::Theme,
};

pub(super) struct TestNotification;

pub struct SettingsView {
    commands: Sender<CoreCommand>,
    draft: Option<AppSettings>,
    committed: Option<AppSettings>,
    data_directory: String,
    pending: bool,
    choosing: bool,
    error: Option<&'static str>,
    scroll: ScrollHandle,
    focus: Vec<FocusHandle>,
    notification_available: bool,
    notification_test_notice: Option<&'static str>,
}

impl EventEmitter<TestNotification> for SettingsView {}

impl SettingsView {
    pub fn new(commands: Sender<CoreCommand>, cx: &mut Context<Self>) -> Self {
        let scroll = ScrollHandle::new();
        Self {
            commands,
            draft: None,
            committed: None,
            data_directory: String::new(),
            pending: false,
            choosing: false,
            error: None,
            focus: (0..9).map(|_| cx.focus_handle().tab_stop(true)).collect(),
            notification_available: false,
            notification_test_notice: None,
            scroll,
        }
    }

    pub fn set_notification_available(&mut self, available: bool, cx: &mut Context<Self>) {
        self.notification_available = available;
        cx.notify();
    }

    pub fn initialize(
        &mut self,
        value: AppSettings,
        data_directory: String,
        cx: &mut Context<Self>,
    ) {
        self.committed = Some(value.clone());
        self.draft = Some(value);
        self.data_directory = data_directory;
        cx.notify();
    }

    pub fn complete(&mut self, result: Result<AppSettings, SettingsError>, cx: &mut Context<Self>) {
        self.pending = false;
        self.error = match result {
            Ok(value) => {
                self.committed = Some(value.clone());
                self.draft = Some(value);
                None
            }
            Err(SettingsError::Directory) => {
                Some("Папка недоступна для записи. Выберите существующую папку и сохраните снова.")
            }
            Err(SettingsError::Storage) => {
                Some("Не удалось сохранить настройки в SQLite. Предыдущие настройки не изменены.")
            }
            Err(SettingsError::Busy) => Some(
                "Дождитесь завершения текущего файла или отмените его, затем сохраните настройки.",
            ),
        };
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.pending = false;
        self.error = Some("Фоновый сервис остановлен. Перезапустите приложение.");
        cx.notify();
    }

    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_notifications(
        &mut self,
        available: bool,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.notification_available = available;
        if let Some(draft) = &mut self.draft {
            draft.system_notifications = enabled;
        }
        self.committed = self.draft.clone();
        self.error = None;
        self.pending = false;
        if available {
            self.scroll.scroll_to_bottom();
        } else {
            self.scroll.scroll_to_item(2);
        }
        cx.notify();
    }

    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = &mut self.draft {
            draft.download_directory = "/Users/demo/Книги/Telegram".into();
            draft.formats = draft
                .formats
                .toggle(BookFormat::Zip)
                .unwrap_or(draft.formats);
        }
        cx.notify();
    }

    fn dirty(&self) -> bool {
        self.draft != self.committed
    }

    pub fn reveal_focus(&self, window: &Window, cx: &mut Context<Self>) {
        if let Some(index) = self.focus.iter().position(|focus| focus.is_focused(window)) {
            // Direct children are destination, formats, notifications, and details.
            self.scroll.scroll_to_item(if index == 0 {
                0
            } else if index <= 6 {
                1
            } else {
                2
            });
            cx.notify();
        }
    }

    fn step_focus(&self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.focus.iter().position(|focus| focus.is_focused(window));
        let next = current.and_then(|index| {
            if reverse {
                index.checked_sub(1)
            } else {
                (index + 1 < self.focus.len()).then_some(index + 1)
            }
        });
        if let Some(next) = next {
            self.focus[next].focus(window, cx);
        } else if reverse {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        self.reveal_focus(window, cx);
    }

    fn toggle(&mut self, format: BookFormat, cx: &mut Context<Self>) {
        if self.pending || self.choosing {
            return;
        }
        if let Some(draft) = &mut self.draft {
            if let Some(formats) = draft.formats.toggle(format) {
                draft.formats = formats;
                self.error = None;
            } else {
                self.error = Some("Оставьте хотя бы один разрешённый формат.");
            }
        }
        cx.notify();
    }

    fn request_notification_test(&mut self, cx: &mut Context<Self>) {
        self.notification_test_notice = Some(if self.notification_available {
            "Запрос передан ОС. Если баннер не появился, проверьте разрешения и режим фокусирования. После выдачи разрешения можно нажать «Тест» ещё раз."
        } else {
            "Системный тест недоступен в этом запуске. На macOS запустите приложение через sh scripts/run-macos.sh."
        });
        if self.notification_available {
            cx.emit(TestNotification);
        }
        cx.notify();
    }

    fn toggle_notifications(&mut self, cx: &mut Context<Self>) {
        if self.pending || self.choosing {
            return;
        }
        if let Some(draft) = &mut self.draft {
            draft.system_notifications = !draft.system_notifications;
            self.error = None;
        }
        cx.notify();
    }

    fn choose(&mut self, cx: &mut Context<Self>) {
        if self.pending || self.choosing {
            return;
        }
        self.choosing = true;
        self.error = None;
        let response = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Папка загрузок по умолчанию".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = response.await;
            let _ = view.update(cx, |view, cx| {
                view.choosing = false;
                match result {
                    Ok(Ok(Some(paths))) if paths.len() == 1 => {
                        if let Some(draft) = &mut view.draft {
                            draft.download_directory = paths[0].clone();
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => view.error = Some("Не удалось выбрать папку. Повторите действие."),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.pending || self.choosing || !self.dirty() {
            return;
        }
        if let Some(draft) = self.draft.clone() {
            match self.commands.try_send(CoreCommand::SaveSettings(draft)) {
                Ok(()) => {
                    self.pending = true;
                    self.error = None;
                }
                Err(_) => {
                    self.error = Some("Не удалось отправить настройки. Перезапустите приложение.")
                }
            }
        }
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let mut root = div()
            .id("settings-form")
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .gap_4()
            .on_action(cx.listener(|view, _: &super::FocusNext, window, cx| {
                view.step_focus(false, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|view, _: &super::FocusPrevious, window, cx| {
                view.step_focus(true, window, cx);
                cx.stop_propagation();
            }));
        let Some(draft) = &self.draft else {
            return root.child("Загружаем настройки…");
        };
        let directory = draft.download_directory.to_string_lossy().into_owned();
        let formats = draft.formats;
        let notifications = draft.system_notifications;
        let busy = self.pending || self.choosing;
        root = root.child(div().id("settings-body").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll)
            .child(div().flex_shrink_0().flex().flex_col().gap_3().pb_5()
                .child(div().text_lg().child("Папка загрузок"))
                .child(div().text_sm().text_color(theme.muted).child("Новые книги сохраняются сюда без повторного выбора папки. Для отдельной группы можно указать другую папку в «Каналах»."))
                .child(div().flex().items_center().gap_3().p_3().bg(theme.sidebar).rounded_md()
                    .child(Icon::Channels.element())
                    .child(div().flex_1().min_w_0().truncate().text_ellipsis_middle().child(directory)))
                .when(!busy, |section| section.child(button("settings-choose-directory", "Выбрать папку…")
                    .debug_selector(|| "settings-directory-control".into())
                    .track_focus(&self.focus[0])
                    .on_click(cx.listener(|view, _, _, cx| view.choose(cx)))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) { view.choose(cx); cx.stop_propagation(); }
                    })))))
            .child(div().flex_shrink_0().flex().flex_col().gap_3().py_5().border_t_1().border_color(theme.border)
                .child(div().text_lg().child("Разрешённые форматы"))
                .child(div().text_sm().text_color(theme.muted).child("Определяют, какие вложения находить в каналах. После изменения результаты открытого канала загружаются заново. Принятые задания и библиотека не меняются."))
                .child(div().flex().flex_wrap().gap_2().children(BookFormat::ALL.into_iter().enumerate().map(|(index, format)| {
                    button(("settings-format", index), format.extension().to_uppercase())
                        .debug_selector(move || format!("settings-format-{index}"))
                        .track_focus(&self.focus[index + 1])
                        .aria_selected(formats.contains(format))
                        .when(formats.contains(format), |chip| chip.bg(theme.selection).border_color(theme.accent))
                        .when(formats.contains(format), |chip| chip.child(Icon::Check.element()))
                        .on_click(cx.listener(move |view, _, _, cx| view.toggle(format, cx)))
                        .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) { view.toggle(format, cx); cx.stop_propagation(); }
                        }))
                }))))
            .child(div().flex_shrink_0().flex().flex_col().gap_3().py_5().border_t_1().border_color(theme.border)
                .child(div().text_lg().child("Системные уведомления"))
                .child(div().text_sm().text_color(theme.muted).child("Одна сводка о новых книгах, только когда окно неактивно. Разрешение запрашивает ОС при первой отправке; её настройки могут запретить показ."))
                .child(div().flex().flex_wrap().items_center().gap_2()
                    .child(switch("settings-notifications", "Отправлять уведомления", notifications)
                    .debug_selector(|| "settings-notifications-control".into()).track_focus(&self.focus[7])
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_notifications(cx)))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) { view.toggle_notifications(cx); cx.stop_propagation(); }
                    })))
                    .child(button("test-notifications", "Тест")
                        .debug_selector(|| "settings-notifications-test".into())
                        .aria_label("Отправить тестовое системное уведомление").track_focus(&self.focus[8])
                        .on_click(cx.listener(|view, _, _, cx| view.request_notification_test(cx)))
                        .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) { view.request_notification_test(cx); cx.stop_propagation(); }
                        }))))
                .child(div().text_size(gpui::px(11.)).text_color(theme.muted)
                    .child("«Тест» отправляет одно уведомление сразу, без сохранения настроек и даже при активном окне."))
                .when_some(self.notification_test_notice, |section, notice| section.child(
                    div().id("notification-test-status").role(Role::Status).aria_label(notice).text_sm().text_color(theme.muted).child(notice)
                ))
                .when(!self.notification_available, |section| section.child(div().text_sm().text_color(theme.muted)
                    .child("Для баннеров на macOS запустите .app через sh scripts/run-macos.sh. При cargo run находки и счётчик работают без баннеров."))))
            .child(setting("Директория данных приложения", self.data_directory.clone()).flex_shrink_0())
            .child(setting("Загрузка", "По одному файлу. Восстановленная очередь ждёт ручного продолжения.").flex_shrink_0()));
        root.child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .flex_shrink_0()
                .border_t_1()
                .border_color(theme.border)
                .pt_3()
                .when_some(self.error, |footer, error| {
                    footer.child(
                        div()
                            .id("settings-error")
                            .role(Role::Alert)
                            .aria_label(error)
                            .text_sm()
                            .text_color(theme.error)
                            .child(error),
                    )
                })
                .child(if !busy && self.dirty() && !self.commands.is_closed() {
                    primary_button("save-settings", "Сохранить настройки")
                        .on_click(cx.listener(|view, _, _, cx| view.save(cx)))
                        .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) {
                                view.save(cx);
                                cx.stop_propagation();
                            }
                        }))
                        .into_any_element()
                } else {
                    div()
                        .text_sm()
                        .text_color(theme.muted)
                        .child(if self.pending {
                            "Сохраняем настройки…"
                        } else if self.choosing {
                            "Выберите папку в системном диалоге…"
                        } else if self.commands.is_closed() {
                            "Сервис недоступен"
                        } else {
                            "Нет несохранённых изменений"
                        })
                        .into_any_element()
                }),
        )
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn notification_switch_supports_space_and_enter_without_saving(cx: &mut TestAppContext) {
        use gpui::{VisualTestContext, px, size};
        let (ui, backend) = tgbooks_core::connect();
        let window = cx.open_window(size(px(440.), px(300.)), |window, cx| {
            super::super::bind_keys(cx);
            let mut view = SettingsView::new(ui.commands, cx);
            view.initialize(AppSettings::new(std::env::temp_dir()), "data".into(), cx);
            view.focus[7].focus(window, cx);
            view
        });
        let Ok(view) = window.root(cx) else {
            panic!("settings window missing");
        };
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        for (keys, enabled) in [("space", true), ("enter", false)] {
            visual.simulate_keystrokes(keys);
            visual.run_until_parked();
            view.read_with(&visual.cx, |view, _| {
                assert_eq!(
                    view.draft
                        .as_ref()
                        .map(|settings| settings.system_notifications),
                    Some(enabled)
                );
                assert_eq!(
                    view.committed
                        .as_ref()
                        .map(|settings| settings.system_notifications),
                    Some(false)
                );
                assert_eq!(view.dirty(), enabled);
            });
        }
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn folder_dialog_and_save_are_explicit_atomic_and_retryable(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| SettingsView::new(ui.commands, cx));
        let before = AppSettings::new(std::env::temp_dir());
        view.update(cx, |view, cx| {
            view.initialize(before.clone(), "data".into(), cx);
            view.choose(cx);
        });
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(!view.dirty()));
        let directory = std::env::temp_dir().join("Книги 世界");
        view.update(cx, |view, cx| view.choose(cx));
        cx.simulate_path_prompt_response(|_| Some(vec![directory.clone()]));
        cx.run_until_parked();
        assert!(
            backend.commands.try_recv().is_err(),
            "choosing a folder must not save prematurely"
        );
        view.update(cx, |view, cx| {
            view.save(cx);
            view.save(cx);
        });
        let Ok(CoreCommand::SaveSettings(value)) = backend.commands.try_recv() else {
            panic!("save command missing");
        };
        assert_eq!(value.download_directory, directory);
        assert!(backend.commands.try_recv().is_err());
        view.update(cx, |view, cx| {
            view.complete(Err(SettingsError::Directory), cx);
            assert_eq!(view.committed, Some(before));
            assert!(view.dirty());
            assert_eq!(
                view.draft.as_ref().map(|value| &value.download_directory),
                Some(&directory)
            );
            view.save(cx);
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SaveSettings(_))
        ));
        view.update(cx, |view, cx| {
            view.complete(Ok(value.clone()), cx);
            assert!(!view.dirty());
            assert_eq!(view.committed, Some(value));
        });
    }

    #[gpui::test]
    fn keyboard_focus_scrolls_preferences_into_view_in_small_windows(cx: &mut TestAppContext) {
        use gpui::{VisualTestContext, px, size};
        let (ui, _backend) = tgbooks_core::connect();
        let window = cx.open_window(size(px(440.), px(300.)), |window, cx| {
            super::super::bind_keys(cx);
            let mut view = SettingsView::new(ui.commands, cx);
            view.initialize(AppSettings::new(std::env::temp_dir()), "data".into(), cx);
            view.focus[0].focus(window, cx);
            view
        });
        let Ok(view) = window.root(cx) else {
            panic!("settings window");
        };
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        for (index, selector, keys) in [
            (6, "settings-format-5", "tab tab tab tab tab tab"),
            (7, "settings-notifications-control", "tab"),
            (8, "settings-notifications-test", "tab"),
            (
                0,
                "settings-directory-control",
                "shift-tab shift-tab shift-tab shift-tab shift-tab shift-tab shift-tab shift-tab",
            ),
        ] {
            let focus = view.read_with(&visual.cx, |view, _| view.focus[index].clone());
            visual.simulate_keystrokes(keys);
            visual.run_until_parked();
            let Some(bounds) = visual.debug_bounds(selector) else {
                panic!("missing settings control");
            };
            visual
                .update(|window, _| assert!(focus.is_focused(window), "focused control was lost"));
            view.read_with(&visual.cx, |view, _| {
                assert!(
                    bounds.top() >= view.scroll.bounds().top() - px(1.),
                    "{index}: {bounds:?}, viewport {:?}, offset {:?}",
                    view.scroll.bounds(),
                    view.scroll.offset()
                );
                assert!(
                    bounds.bottom() <= view.scroll.bounds().bottom() + px(1.),
                    "{index}: {bounds:?}, viewport {:?}, offset {:?}",
                    view.scroll.bounds(),
                    view.scroll.offset()
                );
            });
        }
    }

    #[gpui::test]
    fn inline_test_sends_immediately_without_enabling_or_saving_automatic_notifications(
        cx: &mut TestAppContext,
    ) {
        use super::super::{BookApp, Screen};
        use gpui::{VisualTestContext, px, size};
        use tgbooks_core::{AppInfo, CoreEvent};
        let (ui, backend) = tgbooks_core::connect();
        cx.update(|cx| cx.set_app_identity(crate::platform::APP_ID, crate::platform::APP_NAME));
        let window = cx.open_window(size(px(760.), px(520.)), |window, cx| {
            super::super::bind_keys(cx);
            let mut app = BookApp::new(ui, cx);
            app.notification_available = true;
            app.apply_event(
                CoreEvent::Ready(AppInfo {
                    settings: AppSettings::new(std::env::temp_dir()),
                    data_directory: "data".into(),
                    download_directory: "books".into(),
                }),
                cx,
            );
            app.navigate(Screen::Settings, cx);
            app.preferences.update(cx, |view, cx| {
                view.set_notification_available(true, cx);
                view.focus[7].focus(window, cx);
            });
            app
        });
        let Ok(app) = window.root(cx) else {
            panic!("window missing");
        };
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        visual.simulate_keystrokes("tab");
        let Some(toggle) = visual.debug_bounds("settings-notifications-control") else {
            panic!("toggle missing");
        };
        let Some(test) = visual.debug_bounds("settings-notifications-test") else {
            panic!("test missing");
        };
        assert!((toggle.center().y - test.center().y).abs() <= px(1.));
        assert!(toggle.right() <= test.left());
        assert!(test.top() >= px(0.) && test.bottom() <= px(520.));
        visual.simulate_keystrokes("enter");
        assert_eq!(visual.cx.shown_system_notifications().len(), 1);
        app.read_with(&visual.cx, |app, cx| {
            let preferences = app.preferences.read(cx);
            assert!(!preferences.dirty());
            assert!(
                preferences
                    .committed
                    .as_ref()
                    .is_some_and(|settings| !settings.system_notifications)
            );
            assert!(preferences.notification_test_notice.is_some());
        });
        app.update(&mut visual.cx, |app, cx| {
            app.preferences
                .update(cx, |view, cx| view.toggle_notifications(cx))
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        assert_eq!(
            visual.cx.shown_system_notifications().len(),
            2,
            "unsaved changes must not hide the test"
        );
        assert!(
            backend.commands.try_recv().is_err(),
            "testing must not save settings or call Telegram"
        );
    }

    #[gpui::test]
    fn unavailable_notification_backend_explains_the_problem_without_emitting_a_test(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| SettingsView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            view.initialize(AppSettings::new(std::env::temp_dir()), "data".into(), cx);
            view.request_notification_test(cx);
            assert!(
                view.notification_test_notice
                    .is_some_and(|notice| notice.contains("sh scripts/run-macos.sh"))
            );
            assert!(!view.dirty());
        });
        assert!(cx.shown_system_notifications().is_empty());
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn notification_opt_in_is_a_draft_until_saved_and_failed_saves_keep_it_retryable(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| SettingsView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            view.initialize(AppSettings::new(std::env::temp_dir()), "data".into(), cx);
            view.toggle_notifications(cx);
            assert!(
                view.draft
                    .as_ref()
                    .is_some_and(|settings| settings.system_notifications)
            );
            assert!(
                view.committed
                    .as_ref()
                    .is_some_and(|settings| !settings.system_notifications)
            );
            assert!(backend.commands.try_recv().is_err());
            view.save(cx);
            let Ok(CoreCommand::SaveSettings(accepted)) = backend.commands.try_recv() else {
                panic!("save missing");
            };
            assert!(accepted.system_notifications);
            view.complete(Err(SettingsError::Storage), cx);
            assert!(view.dirty());
            assert!(
                view.committed
                    .as_ref()
                    .is_some_and(|settings| !settings.system_notifications)
            );
            view.complete(Ok(accepted), cx);
            assert!(!view.dirty());
            assert!(
                view.committed
                    .as_ref()
                    .is_some_and(|settings| settings.system_notifications)
            );
        });
    }

    #[gpui::test]
    fn last_format_cannot_be_disabled(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| SettingsView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            let Some(pdf) = tgbooks_core::BookFormats::from_mask(1) else {
                panic!("pdf");
            };
            view.initialize(
                AppSettings {
                    download_directory: std::env::temp_dir(),
                    formats: pdf,
                    system_notifications: false,
                },
                "data".into(),
                cx,
            );
            view.toggle(BookFormat::Pdf, cx);
            assert!(view.error.is_some());
            assert!(!view.dirty());
            view.toggle(BookFormat::Epub, cx);
            view.save(cx);
        });
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::SaveSettings(value)) if value.formats.mask() == 3)
        );
    }
}
