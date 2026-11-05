use async_channel::Sender;
use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, KeyBinding, Role, ScrollHandle,
    Subscription, Window, actions, div, point, prelude::*, px,
};
use tgbooks_core::{AuthAction, AuthError, AuthState, AuthStep, CoreCommand};

use super::{components::button, input::TextInput, theme::Theme};

actions!(login, [Submit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Submit, Some("TelegramLogin"))]);
}

pub struct LoginForm {
    commands: Sender<CoreCommand>,
    state: AuthState,
    local_error: Option<&'static str>,
    focus: FocusHandle,
    focus_pending: bool,
    scroll: ScrollHandle,
    button_focus: [FocusHandle; 3],
    focus_observers: Vec<Subscription>,
    api_id: Entity<TextInput>,
    api_hash: Entity<TextInput>,
    phone: Entity<TextInput>,
    code: Entity<TextInput>,
    password: Entity<TextInput>,
}

impl LoginForm {
    pub fn new(commands: Sender<CoreCommand>, cx: &mut Context<Self>) -> Self {
        Self {
            commands,
            state: AuthState {
                busy: true,
                ..Default::default()
            },
            local_error: None,
            focus: cx.focus_handle(),
            focus_pending: false,
            scroll: ScrollHandle::default(),
            button_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            focus_observers: Vec::new(),
            api_id: cx.new(|cx| TextInput::new("API ID", "Числовой API ID", false, cx)),
            api_hash: cx.new(|cx| TextInput::new("API hash", "32 символа API hash", true, cx)),
            phone: cx.new(|cx| TextInput::new("Номер телефона", "+7 900 000 00 00", false, cx)),
            code: cx.new(|cx| {
                TextInput::new("Код подтверждения", "Код из Telegram или SMS", false, cx)
            }),
            password: cx
                .new(|cx| TextInput::new("Пароль 2FA", "Облачный пароль Telegram", true, cx)),
        }
    }

    pub fn set_state(&mut self, state: AuthState, cx: &mut Context<Self>) {
        if self.state.step != state.step {
            for input in self.inputs() {
                input.update(cx, |input, cx| input.clear(cx));
            }
        }
        self.focus_pending = self.state.step != state.step || (self.state.busy && !state.busy);
        if self.focus_pending {
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.local_error = None;
        self.state = state;
        cx.notify();
    }

    fn inputs(&self) -> [&Entity<TextInput>; 5] {
        [
            &self.api_id,
            &self.api_hash,
            &self.phone,
            &self.code,
            &self.password,
        ]
    }

    fn watch_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_observers.is_empty() {
            return;
        }
        let handles = self.inputs().map(|input| input.read(cx).focus.clone());
        for (index, handle) in handles.into_iter().enumerate() {
            self.focus_observers
                .push(cx.on_focus(&handle, window, move |form, _, cx| {
                    // Direct children are heading, help, then the visible fields.
                    // Only the API hash is the second field on its step.
                    form.scroll.scroll_to_item(if index == 1 { 3 } else { 2 });
                    cx.notify();
                }));
        }
        for handle in self.button_focus.clone() {
            self.focus_observers
                .push(cx.on_focus(&handle, window, |form, _, cx| {
                    form.scroll.scroll_to_bottom();
                    cx.notify();
                }));
        }
    }

    fn send(&mut self, action: AuthAction, cx: &mut Context<Self>) {
        if self.state.busy {
            return;
        }
        if self
            .commands
            .try_send(CoreCommand::Authenticate(action))
            .is_err()
        {
            self.local_error = Some("Фоновый сервис недоступен. Перезапустите приложение.");
        } else {
            self.state.busy = true;
            self.state.error = None;
            self.local_error = None;
            self.focus_pending = true;
            self.code.update(cx, |input, cx| input.clear(cx));
            self.password.update(cx, |input, cx| input.clear(cx));
        }
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.state.busy
            || self
                .inputs()
                .iter()
                .any(|input| input.read(cx).is_composing())
        {
            return;
        }
        let action = match self.state.step {
            AuthStep::Credentials => AuthAction::Configure {
                api_id: self
                    .api_id
                    .read(cx)
                    .secret()
                    .expose()
                    .trim()
                    .parse()
                    .unwrap_or(0),
                api_hash: self.api_hash.read(cx).secret(),
            },
            AuthStep::Phone => AuthAction::RequestCode(self.phone.read(cx).secret()),
            AuthStep::Code => AuthAction::SubmitCode(self.code.read(cx).secret()),
            AuthStep::Password => AuthAction::SubmitPassword(self.password.read(cx).secret()),
            AuthStep::Authorized => AuthAction::Restore,
        };
        self.send(action, cx);
    }

    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        action: fn() -> AuthAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        button(id, label)
            .flex_shrink_0()
            .track_focus(&self.button_focus[if id == "auth-restart" { 1 } else { 2 }])
            .on_click(cx.listener(move |form, _, _, cx| form.send(action(), cx)))
            .on_key_down(cx.listener(move |form, event, _, cx| {
                if super::is_activation_key(event) {
                    form.send(action(), cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }
}

fn field(label: &'static str, input: &Entity<TextInput>) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_2()
        .child(div().font_weight(gpui::FontWeight::MEDIUM).child(label))
        .child(input.clone())
}

impl Render for LoginForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        self.watch_focus(window, cx);
        if self.focus_pending {
            self.focus_pending = false;
            let focus = if self.state.busy {
                self.focus.clone()
            } else {
                match self.state.step {
                    AuthStep::Credentials => self.api_id.read(cx).focus.clone(),
                    AuthStep::Phone => self.phone.read(cx).focus.clone(),
                    AuthStep::Code => self.code.read(cx).focus.clone(),
                    AuthStep::Password => self.password.read(cx).focus.clone(),
                    AuthStep::Authorized => self.focus.clone(),
                }
            };
            focus.focus(window, cx);
        }
        let root = div()
            .id("telegram-login")
            .key_context("TelegramLogin")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .max_h(gpui::relative(1.))
            .min_h_0()
            .gap_3()
            .max_w(px(440.))
            .on_action(cx.listener(|form, _: &Submit, _, cx| form.submit(cx)));
        if self.state.busy {
            return root
                .child(div().id("auth-status").role(Role::Status).aria_label("Проверяем данные и соединение").text_lg().child("Проверяем данные и соединение…"))
                .child(div().text_color(theme.muted).child(
                    "Если система запросит доступ к защищённому хранилищу, разрешите его. Сетевой запрос может занять до 30 секунд.",
                ));
        }
        let (title, help, submit) = match self.state.step {
            AuthStep::Credentials => (
                "Настройте Telegram API",
                "Получите API ID и hash на my.telegram.org/apps. Данные сохраняются в зашифрованной сессии.",
                "Сохранить и продолжить",
            ),
            AuthStep::Phone => (
                "Войдите в свой аккаунт",
                "Укажите номер в международном формате. Telegram сам определит способ доставки кода.",
                "Получить код",
            ),
            AuthStep::Code => (
                "Введите код подтверждения",
                "Проверьте сообщения Telegram на другом устройстве или SMS. Код не сохраняется на диске.",
                "Подтвердить код",
            ),
            AuthStep::Password => (
                "Подтвердите вход паролем",
                "В аккаунте включена двухэтапная аутентификация. Введите облачный пароль Telegram.",
                "Войти",
            ),
            AuthStep::Authorized => (
                "Telegram подключён",
                "Авторизация сохранена в зашифрованной сессии. После перезапуска повторный ввод кода не требуется, пока сессия действительна.",
                "Проверить сессию",
            ),
        };
        let mut form = div()
            .id("auth-fields")
            .flex()
            .flex_col()
            .min_h_0()
            .gap_3()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .flex_shrink_0()
                    .text_xl()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(title),
            )
            .child(div().flex_shrink_0().text_color(theme.muted).child(help));
        form = match self.state.step {
            AuthStep::Credentials => form
                .child(field("API ID", &self.api_id))
                .child(field("API hash", &self.api_hash)),
            AuthStep::Phone => form.child(field("Номер телефона", &self.phone)),
            AuthStep::Code => form.child(field("Код подтверждения", &self.code)),
            AuthStep::Password => form.child(field("Пароль 2FA", &self.password)),
            AuthStep::Authorized => form,
        };
        if let Some(error) = self
            .local_error
            .map(str::to_owned)
            .or_else(|| self.state.error.map(error_message))
        {
            form = form.child(
                div()
                    .id("auth-error")
                    .role(Role::Alert)
                    .aria_label(error.clone())
                    .flex_shrink_0()
                    .text_color(theme.error)
                    .child(error),
            );
        }
        let mut actions = div().flex().flex_col().flex_shrink_0().gap_3().child(
            button("auth-submit", submit)
                .flex_shrink_0()
                .track_focus(&self.button_focus[0])
                .on_click(cx.listener(|form, _, _, cx| form.submit(cx)))
                .on_key_down(cx.listener(|form, event, _, cx| {
                    if super::is_activation_key(event) {
                        form.submit(cx);
                        cx.stop_propagation();
                    }
                })),
        );
        if matches!(self.state.step, AuthStep::Code | AuthStep::Password) {
            actions = actions.child(self.action_button(
                "auth-restart",
                "Изменить номер",
                || AuthAction::Restart,
                cx,
            ));
        }
        if self.state.error.is_some()
            && (self.state.step == AuthStep::Credentials
                || matches!(
                    self.state.error,
                    Some(AuthError::Network | AuthError::SecureStorage)
                ))
        {
            actions = actions.child(self.action_button(
                "auth-restore",
                "Повторить проверку сохранённой сессии",
                || AuthAction::Restore,
                cx,
            ));
        }
        root.child(form).child(actions)
    }
}

fn error_message(error: AuthError) -> String {
    match error {
        AuthError::InvalidCredentials => "Проверьте API ID и API hash: нужен положительный числовой ID и 32 шестнадцатеричных символа hash.",
        AuthError::InvalidPhone => "Введите номер с + и кодом страны, от 7 до 15 цифр.",
        AuthError::InvalidCode => "Неверный код. Проверьте сообщение Telegram и повторите ввод.",
        AuthError::InvalidPassword => "Неверный облачный пароль. Попробуйте ещё раз.",
        AuthError::CodeExpired => "Код или сессия больше не действуют. Запросите новый код.",
        AuthError::SignUpRequired => "Сначала создайте аккаунт в официальном приложении Telegram.",
        AuthError::FloodWait(seconds) => return format!("Telegram ограничил запросы. Подождите {seconds} секунд, затем повторите действие."),
        AuthError::Network => "Не удалось получить ответ Telegram. Проверьте интернет и повторите действие; для 2FA может потребоваться повторный ввод кода.",
        AuthError::SecureStorage => "Не удалось открыть или сохранить защищённую сессию. Проверьте доступ к Keychain / Credential Manager и директории данных.",
        AuthError::MissingKey => "В системном хранилище отсутствует ключ сессии. Существующий файл не перезаписывается. Восстановите ключ или выберите отдельную директорию данных.",
        AuthError::CorruptSession => "Не удалось расшифровать сессию. Файл не перезаписывается. Восстановите корректную резервную копию или используйте отдельную директорию данных.",
        AuthError::AlreadyRunning => "Сессия занята другим экземпляром приложения. Закройте его и повторите проверку.",
        AuthError::UnsupportedPlatform => "Защищённое хранение поддерживается только на macOS и Windows.",
        AuthError::UnsupportedReply => "Telegram вернул способ входа, который пока не поддерживается Grammers 0.11. Проверьте аккаунт в официальном клиенте.",
        AuthError::Rejected => "Telegram отклонил вход. Проверьте API credentials и состояние аккаунта в официальном клиенте.",
        AuthError::UnexpectedState => "Шаг входа изменился. Вернитесь к вводу номера и повторите попытку.",
    }.to_owned()
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use gpui::{EntityInputHandler as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn form_submits_typed_credentials_once_and_preserves_them_after_an_error(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let (form, cx) = cx.add_window_view(|_, cx| LoginForm::new(ui.commands, cx));
        form.update(&mut cx.cx, |form, cx| {
            form.set_state(AuthState::default(), cx)
        });
        cx.update(|window, cx| {
            form.update(cx, |form, cx| {
                form.api_id.update(cx, |input, cx| {
                    input.replace_text_in_range(None, "1", window, cx)
                });
                form.api_hash.update(cx, |input, cx| {
                    input.replace_text_in_range(None, &"0".repeat(32), window, cx)
                });
                form.submit(cx);
                form.submit(cx);
            });
        });
        let Ok(CoreCommand::Authenticate(AuthAction::Configure { api_id, api_hash })) =
            backend.commands.try_recv()
        else {
            panic!("configuration command was not sent")
        };
        assert_eq!(api_id, 1);
        assert_eq!(api_hash.expose(), "0".repeat(32));
        assert!(backend.commands.try_recv().is_err());
        form.update(&mut cx.cx, |form, cx| {
            form.set_state(
                AuthState {
                    error: Some(AuthError::SecureStorage),
                    ..Default::default()
                },
                cx,
            );
            assert_eq!(form.api_hash.read(cx).secret().expose(), "0".repeat(32));
            form.set_state(
                AuthState {
                    step: AuthStep::Phone,
                    ..Default::default()
                },
                cx,
            );
            assert!(form.api_hash.read(cx).secret().expose().is_empty());
        });
    }

    #[gpui::test]
    fn submitted_2fa_password_is_removed_from_the_field_before_the_reply(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let (form, cx) = cx.add_window_view(|_, cx| LoginForm::new(ui.commands, cx));
        cx.update(|window, cx| {
            form.update(cx, |form, cx| {
                form.set_state(
                    AuthState {
                        step: AuthStep::Password,
                        ..Default::default()
                    },
                    cx,
                );
                form.password.update(cx, |input, cx| {
                    input.replace_text_in_range(None, "dummy password", window, cx)
                });
                form.submit(cx);
                assert!(form.password.read(cx).secret().expose().is_empty());
            });
        });
        let Ok(CoreCommand::Authenticate(AuthAction::SubmitPassword(password))) =
            backend.commands.try_recv()
        else {
            panic!("password command was not sent")
        };
        assert_eq!(password.expose(), "dummy password");
    }

    #[gpui::test]
    fn focus_keeps_fields_and_errors_visible_in_small_forms(cx: &mut TestAppContext) {
        let (ui, _backend) = tgbooks_core::connect();
        let window = cx.open_window(gpui::size(px(440.), px(260.)), |_, cx| {
            LoginForm::new(ui.commands, cx)
        });
        let Ok(form) = window.root(cx) else {
            panic!("login window was not created")
        };
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        form.update(&mut visual.cx, |form, cx| {
            form.set_state(
                AuthState {
                    error: Some(AuthError::SecureStorage),
                    ..Default::default()
                },
                cx,
            );
        });
        visual.run_until_parked();
        let targets = form.read_with(&visual.cx, |form, cx| {
            [
                (form.button_focus[0].clone(), 4),
                (form.api_hash.read(cx).focus.clone(), 3),
                (form.api_id.read(cx).focus.clone(), 2),
            ]
        });
        for (focus, index) in targets {
            visual.update(|window, cx| focus.focus(window, cx));
            visual.run_until_parked();
            // GPUI dispatches focus listeners after paint. Their scroll request
            // takes effect in the following frame.
            assert!(visual.cx.refresh().is_ok());
            visual.run_until_parked();
            visual.update(|window, _| {
                assert!(
                    focus.is_focused(window),
                    "focus moved away from item {index}"
                );
                assert!(window.is_window_active(), "test window is inactive");
            });
            form.read_with(&visual.cx, |form, _| {
                let viewport = form.scroll.bounds();
                let Some(item) = form.scroll.bounds_for_item(index) else {
                    panic!("form item is missing")
                };
                let offset = form.scroll.offset().y;
                assert!(viewport.size.height <= px(260.));
                assert!(item.top() + offset >= viewport.top() - px(1.), "item {index}: viewport {viewport:?}, item {item:?}, offset {offset:?}");
                assert!(item.bottom() + offset <= viewport.bottom() + px(1.), "item {index}: viewport {viewport:?}, item {item:?}, offset {offset:?}, maximum {:?}", form.scroll.max_offset());
            });
        }
    }
}
