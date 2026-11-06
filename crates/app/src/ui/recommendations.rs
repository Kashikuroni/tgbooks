//! Recommendation inbox: all persistence, monitoring and queue admission stay in Core.
use super::{
    channels::{file_size, moved_cursor},
    components::{button, checkbox, empty_state, format_badge, primary_button},
    files,
    input::TextInput,
    is_activation_key,
    select::Select,
    theme::Theme,
};
use async_channel::Sender;
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, Role, UniformListScrollHandle,
    Window, div, prelude::*, px, uniform_list,
};
use std::collections::HashSet;
use tgbooks_core::{
    CatalogState, CoreCommand, DownloadKey, DownloadRecord, DownloadStatus, RecommendationError,
    RecommendationState, RecommendationStatus, TransferState,
};

pub struct GoToSource(pub i64);

pub struct RecommendationsView {
    commands: Sender<CoreCommand>,
    state: RecommendationState,
    query: Entity<TextInput>,
    filter: Entity<Select>,
    marked: HashSet<DownloadKey>,
    cursor: usize,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    queued: HashSet<DownloadKey>,
    completed: HashSet<DownloadKey>,
    sources: HashSet<i64>,
    downloadable: HashSet<i64>,
    pending: bool,
    local_error: Option<&'static str>,
    _observers: Vec<gpui::Subscription>,
}
impl EventEmitter<GoToSource> for RecommendationsView {}

impl RecommendationsView {
    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_selection(&mut self, cx: &mut Context<Self>) {
        self.marked = self
            .state
            .items
            .iter()
            .take(2)
            .map(|item| item.book.key)
            .collect();
        cx.notify();
    }

    pub fn new(commands: Sender<CoreCommand>, cx: &mut Context<Self>) -> Self {
        let query =
            cx.new(|cx| TextInput::search("Поиск новых книг", "Найти книгу или канал…", cx));
        let filter =
            cx.new(|cx| Select::new("", vec!["Все находки".into(), "Непросмотренные".into()], cx));
        let observers = vec![
            cx.observe(&query, |view, _, cx| {
                view.cursor = 0;
                cx.notify();
            }),
            cx.observe(&filter, |view, _, cx| {
                view.cursor = 0;
                cx.notify();
            }),
        ];
        Self {
            commands,
            state: RecommendationState::default(),
            query,
            filter,
            marked: HashSet::new(),
            cursor: 0,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            queued: HashSet::new(),
            completed: HashSet::new(),
            sources: HashSet::new(),
            downloadable: HashSet::new(),
            pending: false,
            local_error: None,
            _observers: observers,
        }
    }

    pub fn set_state(&mut self, state: RecommendationState, cx: &mut Context<Self>) {
        if state.account != self.state.account {
            self.marked.clear();
            self.cursor = 0;
            self.pending = false;
            if state.account.is_none() {
                self.sources.clear();
                self.downloadable.clear();
            }
        }
        self.marked.retain(|key| {
            state
                .items
                .iter()
                .any(|item| item.book.key == *key && item.status != RecommendationStatus::Hidden)
        });
        self.state = state;
        self.local_error = None;
        cx.notify();
    }
    pub fn finish_command(&mut self, cx: &mut Context<Self>) {
        self.pending = false;
        cx.notify();
    }
    pub fn set_catalog(&mut self, state: &CatalogState, cx: &mut Context<Self>) {
        if self.state.account.is_some() && self.state.account != state.sources.account {
            self.set_state(RecommendationState::default(), cx);
        }
        self.sources = state.channels.iter().map(|source| source.id).collect();
        self.downloadable = state
            .channels
            .iter()
            .filter(|source| !source.protected)
            .map(|source| source.id)
            .collect();
        cx.notify();
    }
    pub fn set_downloads(
        &mut self,
        records: &[DownloadRecord],
        transfer: &TransferState,
        cx: &mut Context<Self>,
    ) {
        self.completed = records
            .iter()
            .filter(|record| record.status == DownloadStatus::Completed)
            .map(|record| record.key)
            .collect();
        self.queued = transfer
            .queued
            .iter()
            .map(|job| job.book.key)
            .chain(transfer.active)
            .collect();
        cx.notify();
    }
    pub fn unread_count(&self) -> usize {
        self.state
            .items
            .iter()
            .filter(|item| {
                item.status == RecommendationStatus::Unread
                    && !self.completed.contains(&item.book.key)
            })
            .count()
    }
    pub(super) fn account(&self) -> Option<i64> {
        self.state.account
    }
    pub(super) fn notification_keys(&self) -> HashSet<DownloadKey> {
        self.state
            .items
            .iter()
            .filter(|item| {
                item.status == RecommendationStatus::Unread
                    && !self.completed.contains(&item.book.key)
            })
            .map(|item| item.book.key)
            .collect()
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.send(CoreCommand::ReloadRecommendations, cx);
    }

    fn visible(&self, cx: &gpui::App) -> Vec<usize> {
        self.state
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.status != RecommendationStatus::Hidden
                    && !self.completed.contains(&item.book.key)
                    && (self.filter.read(cx).selected == 0
                        || item.status == RecommendationStatus::Unread)
                    && files::matches(
                        &item.book.file_name,
                        Some(&item.source_title),
                        self.query.read(cx).text(),
                        None,
                    )
            })
            .map(|(index, _)| index)
            .collect()
    }
    fn keys(&self) -> Vec<DownloadKey> {
        self.state
            .items
            .iter()
            .filter(|item| {
                self.marked.contains(&item.book.key)
                    && !self.completed.contains(&item.book.key)
                    && item.status != RecommendationStatus::Hidden
            })
            .map(|item| item.book.key)
            .collect()
    }
    fn send(&mut self, command: CoreCommand, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        match self.commands.try_send(command) {
            Ok(()) => {
                self.pending = true;
                self.local_error = None;
            }
            Err(_) => {
                self.local_error = Some("Не удалось отправить команду. Перезапустите приложение.")
            }
        }
        cx.notify();
    }
    fn toggle_monitoring(&mut self, cx: &mut Context<Self>) {
        if let Some(account) = self.state.account {
            self.send(
                CoreCommand::SetMonitoring {
                    account,
                    enabled: !self.state.monitoring,
                },
                cx,
            );
        }
    }
    fn change(&mut self, status: RecommendationStatus, cx: &mut Context<Self>) {
        let keys = self.keys();
        if let Some(account) = self.state.account
            && !keys.is_empty()
        {
            self.send(
                CoreCommand::ChangeRecommendations {
                    account,
                    keys,
                    status,
                },
                cx,
            );
        }
    }
    fn download(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<_> = self
            .keys()
            .into_iter()
            .filter(|key| !self.queued.contains(key) && self.downloadable.contains(&key.channel_id))
            .collect();
        if let Some(account) = self.state.account
            && !keys.is_empty()
        {
            self.send(CoreCommand::DownloadRecommendations { account, keys }, cx);
        }
    }
}

impl Render for RecommendationsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let indices = self.visible(cx);
        self.cursor = self.cursor.min(indices.len().saturating_sub(1));
        let mut root = div().flex().flex_col().h_full().min_h_0().gap_3();
        if self.state.account.is_none() {
            return root.child(empty_state(
                "Подключите Telegram",
                "Рекомендации сохраняются отдельно для каждого аккаунта.",
            ));
        }
        let status = if let Some(channel) = &self.state.checking {
            format!("Проверяем: {channel}")
        } else if self.state.monitoring {
            "Мониторинг включён. Проверки выполняются между загрузками.".into()
        } else {
            "Мониторинг выключен. Найденные книги сохранены.".into()
        };
        root = root.child(div().flex().flex_wrap().items_center().gap_3().flex_shrink_0()
            .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted).child(status))
            .when(!self.pending && !self.commands.is_closed(), |row| row.child(
                button("toggle-monitoring", if self.state.monitoring { "Выключить мониторинг" } else { "Включить мониторинг" })
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_monitoring(cx)))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) { view.toggle_monitoring(cx); cx.stop_propagation(); }
                    }))
            )))
            .child(div().flex_shrink_0().text_size(px(11.)).text_color(theme.muted)
                .child("Первое включение пропускает старую историю. Файлы не скачиваются без вашего выбора."));
        if let Some(message) = self
            .local_error
            .map(str::to_owned)
            .or_else(|| self.state.error.map(error_message))
        {
            root = root.child(
                div()
                    .id("recommendation-error")
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .text_sm()
                    .text_color(theme.error)
                    .flex_shrink_0()
                    .child(message)
                    .when(!self.pending, |row| {
                        row.child(
                            button("reload-recommendations", "Обновить находки")
                                .on_click(cx.listener(|view, _, _, cx| view.refresh(cx)))
                                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        view.refresh(cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        )
                    }),
            );
        }
        root = root.child(
            div()
                .flex()
                .gap_3()
                .flex_shrink_0()
                .child(div().flex_1().min_w_0().child(self.query.clone()))
                .child(self.filter.clone()),
        );
        if indices.is_empty() {
            root = root.child(div().flex_1().min_h_0().child(empty_state(
                if !self.state.loaded && self.state.error.is_some() { "Не удалось загрузить находки" }
                else if self.state.loaded { "Новых книг пока нет" } else { "Загружаем находки…" },
                "Добавьте каналы в «Мои источники» и включите мониторинг. Проверьте также поиск и фильтр."
            )));
        } else {
            root = root.child(div().id("recommendation-list").role(Role::ListBox).aria_label("Новые книги. Стрелки перемещают курсор, пробел отмечает, Enter добавляет в очередь")
                .track_focus(&self.focus).flex_1().min_h_0().overflow_hidden()
                .border_1().border_color(theme.border).focus(move |style| style.border_color(theme.accent))
                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                    let visible = view.visible(cx);
                    if event.keystroke.key == "enter" { view.download(cx); cx.stop_propagation(); }
                    else if event.keystroke.key == "space" {
                        if let Some(&index) = visible.get(view.cursor) {
                            let key = view.state.items[index].book.key;
                            if !view.marked.insert(key) { view.marked.remove(&key); }
                            cx.notify();
                        }
                        cx.stop_propagation();
                    } else if let Some(next) = moved_cursor(&event.keystroke.key, view.cursor, visible.len()) {
                        view.cursor = next; view.scroll.scroll_to_item(next, gpui::ScrollStrategy::Nearest); cx.notify(); cx.stop_propagation();
                    }
                }))
                .child(uniform_list("recommendation-rows", indices.len(), cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                    let indices = view.visible(cx);
                    range.map(|position| {
                        let item = &view.state.items[indices[position]];
                        let key = item.book.key;
                        let selected = view.marked.contains(&key);
                        let disposition = if view.queued.contains(&key) { "В очереди" }
                            else if item.status == RecommendationStatus::Unread { "Новое" } else { "Просмотрено" };
                        let metadata = format!("{} · {} · {disposition}", item.source_title, files::date(item.message_date));
                        div().id(("recommendation", position)).role(Role::ListBoxOption)
                            .aria_label(format!("{}. {}. {}. {metadata}", item.book.file_name, item.book.format.extension(), file_size(item.book.size_bytes))).aria_selected(selected)
                            .aria_position_in_set(position + 1).aria_size_of_set(indices.len())
                            .h(px(72.)).w_full().px_3().flex().items_center().gap_3()
                            .border_b_1().border_color(theme.border)
                            .when(selected || position == view.cursor && view.focus.is_focused(window), |row| row.bg(theme.selection))
                            .cursor_pointer().hover(move |style| style.bg(theme.selected))
                            .child(checkbox(("recommendation-check", position), "Выбрать рекомендацию", selected, false).tab_stop(false))
                            .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
                                .child(div().truncate().text_ellipsis_middle().child(item.book.file_name.clone()))
                                .child(div().text_size(px(11.)).text_color(theme.muted).truncate().child(metadata)))
                            .child(format_badge(Some(item.book.format)))
                            .child(div().w(px(72.)).flex_shrink_0().text_sm().text_right().text_color(theme.muted).truncate()
                                .child(item.book.size_bytes.map(|size| file_size(Some(size))).unwrap_or_else(|| "—".into())))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.focus.focus(window, cx); view.cursor = position;
                                if !view.marked.insert(key) { view.marked.remove(&key); }
                                cx.notify();
                            }))
                    }).collect::<Vec<_>>()
                })).track_scroll(&self.scroll).h_full().w_full()));
        }
        let keys = self.keys();
        let can_download = keys
            .iter()
            .any(|key| !self.queued.contains(key) && self.downloadable.contains(&key.channel_id));
        root.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .flex_shrink_0()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted)
                        .child(if self.pending {
                            "Выполняем…".into()
                        } else {
                            format!(
                                "Непросмотренных: {} · Выбрано: {}",
                                self.unread_count(),
                                keys.len()
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when(!self.pending && !keys.is_empty(), |row| {
                            row.child(
                                button("read-recommendations", "Просмотрено")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.change(RecommendationStatus::Read, cx)
                                    }))
                                    .on_key_down(cx.listener(
                                        |view, event: &KeyDownEvent, _, cx| {
                                            if is_activation_key(event) {
                                                view.change(RecommendationStatus::Read, cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    )),
                            )
                            .child(
                                button("hide-recommendations", "Скрыть")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.change(RecommendationStatus::Hidden, cx)
                                    }))
                                    .on_key_down(cx.listener(
                                        |view, event: &KeyDownEvent, _, cx| {
                                            if is_activation_key(event) {
                                                view.change(RecommendationStatus::Hidden, cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    )),
                            )
                            .when(can_download, |row| {
                                row.child(
                                    primary_button(
                                        "download-recommendations",
                                        "Добавить в очередь",
                                    )
                                    .debug_selector(|| "recommendation-download".into())
                                    .on_click(cx.listener(|view, _, _, cx| view.download(cx)))
                                    .on_key_down(cx.listener(
                                        |view, event: &KeyDownEvent, _, cx| {
                                            if is_activation_key(event) {
                                                view.download(cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    )),
                                )
                            })
                            .when(
                                keys.len() == 1 && self.sources.contains(&keys[0].channel_id),
                                |row| {
                                    let id = keys[0].channel_id;
                                    row.child(
                                        button("recommendation-source", "К каналу")
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.emit(GoToSource(id))
                                            }))
                                            .on_key_down(cx.listener(
                                                move |_, event: &KeyDownEvent, _, cx| {
                                                    if is_activation_key(event) {
                                                        cx.emit(GoToSource(id));
                                                        cx.stop_propagation();
                                                    }
                                                },
                                            )),
                                    )
                                },
                            )
                        })
                        .when(!self.pending && !can_download && !keys.is_empty(), |row| {
                            row.child(
                                div().text_sm().text_color(theme.muted).child(
                                    "Уже в очереди либо источник недоступен для скачивания.",
                                ),
                            )
                        }),
                ),
        )
    }
}

fn error_message(error: RecommendationError) -> String {
    match error {
        RecommendationError::Storage => "Не удалось прочитать или сохранить находки. Повторите загрузку; сохранённые данные не удалены.".into(),
        RecommendationError::Unauthorized => "Подключите Telegram заново. Мониторинг не переносится между аккаунтами.".into(),
        RecommendationError::NoSources => "Сначала добавьте каналы в «Мои источники».".into(),
        RecommendationError::Unavailable => "Находка или канал недоступны. Обновите каналы и список находок.".into(),
        RecommendationError::Download(error) => super::downloads::error_message(error),
        RecommendationError::Telegram(error) => format!("Мониторинг: {}", super::channels::error_message(error, true)),
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use tgbooks_core::{BookAttachment, BookFormat, Recommendation, SavedSources};

    fn state() -> RecommendationState {
        RecommendationState {
            account: Some(1),
            loaded: true,
            items: (1..=2)
                .map(|message_id| Recommendation {
                    book: BookAttachment {
                        key: DownloadKey {
                            channel_id: -1,
                            message_id,
                            document_id: i64::from(message_id),
                        },
                        file_name: format!("Книга {message_id} 世界.epub"),
                        size_bytes: Some(3),
                        format: BookFormat::Epub,
                    },
                    source_title: "Books".into(),
                    message_date: 1_794_000_000,
                    status: RecommendationStatus::Unread,
                })
                .collect(),
            ..Default::default()
        }
    }
    fn catalog() -> CatalogState {
        CatalogState {
            authenticated: true,
            channels: vec![tgbooks_core::ChannelSummary {
                id: -1,
                title: "Books".into(),
                username: None,
                protected: false,
            }],
            sources: SavedSources {
                account: Some(1),
                loaded: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[gpui::test]
    fn discovery_is_not_consent_and_keyboard_selection_sends_one_explicit_group(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = RecommendationsView::new(ui.commands, cx);
            view.set_catalog(&catalog(), cx);
            view.set_state(state(), cx);
            view.focus.focus(window, cx);
            view
        });
        assert!(backend.commands.try_recv().is_err());
        view.update(&mut cx.cx, |view, cx| view.download(cx));
        assert!(
            backend.commands.try_recv().is_err(),
            "no checked rows is not consent"
        );
        cx.simulate_keystrokes("space down space enter");
        let Ok(CoreCommand::DownloadRecommendations { account, keys }) =
            backend.commands.try_recv()
        else {
            panic!("download action missing");
        };
        assert_eq!(account, 1);
        assert_eq!(keys.len(), 2);
        assert!(backend.commands.try_recv().is_err());
        view.update(&mut cx.cx, |view, cx| {
            let mut polling = state();
            polling.checking = Some("Books".into());
            view.set_state(polling, cx);
            assert!(
                view.pending,
                "monitoring updates are not command acknowledgements"
            );
            view.finish_command(cx);
            assert!(!view.pending);
        });
    }

    #[gpui::test]
    fn read_and_hidden_states_are_independent_and_an_account_change_clears_the_inbox(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| RecommendationsView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            view.set_catalog(&catalog(), cx);
            view.set_state(state(), cx);
            assert_eq!(view.unread_count(), 2);
            let key = view.state.items[0].book.key;
            view.marked.insert(key);
            view.change(RecommendationStatus::Read, cx);
            assert_eq!(
                view.unread_count(),
                2,
                "read status requires durable acknowledgement"
            );
            let mut updated = state();
            updated.items[0].status = RecommendationStatus::Read;
            view.set_state(updated.clone(), cx);
            view.finish_command(cx);
            assert_eq!(view.unread_count(), 1);
            assert_eq!(
                view.visible(cx).len(),
                2,
                "reading does not delete the recommendation"
            );
            updated.items[0].status = RecommendationStatus::Hidden;
            view.set_state(updated, cx);
            assert_eq!(view.visible(cx).len(), 1);
            let mut other = catalog();
            other.sources.account = Some(2);
            other.channels.clear();
            view.set_catalog(&other, cx);
            assert!(view.state.items.is_empty() && view.marked.is_empty());
            assert_eq!(view.unread_count(), 0);
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::ChangeRecommendations {
                status: RecommendationStatus::Read,
                ..
            })
        ));
    }

    #[gpui::test]
    fn selected_actions_remain_visible_and_keyboard_operable_in_a_small_window(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let window = cx.open_window(gpui::size(px(440.), px(300.)), |window, cx| {
            let mut view = RecommendationsView::new(ui.commands, cx);
            view.set_catalog(&catalog(), cx);
            view.set_state(state(), cx);
            view.preview_selection(cx);
            view.focus.focus(window, cx);
            view
        });
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        let Some(bounds) = visual.debug_bounds("recommendation-download") else {
            panic!("download action missing");
        };
        assert!(
            bounds.top() >= px(0.) && bounds.bottom() <= px(300.),
            "{bounds:?}"
        );
        visual.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
            window.focus_next(cx);
        });
        visual.simulate_keystrokes("enter");
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::DownloadRecommendations { account: 1, keys }) if keys.len() == 2)
        );
    }

    #[gpui::test]
    fn queued_and_protected_books_cannot_be_added_again(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| RecommendationsView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            let mut protected = catalog();
            protected.channels[0].protected = true;
            view.set_catalog(&protected, cx);
            view.set_state(state(), cx);
            let key = view.state.items[0].book.key;
            view.marked.insert(key);
            view.download(cx);
            view.set_catalog(&catalog(), cx);
            view.queued.insert(key);
            view.download(cx);
        });
        assert!(backend.commands.try_recv().is_err());
    }
}
