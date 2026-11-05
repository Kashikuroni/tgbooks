//! Channel and attachment views consume only Core's display models and commands.

use std::collections::HashSet;

use async_channel::Sender;
use gpui::{
    Context, Div, FocusHandle, KeyDownEvent, Role, ScrollStrategy, Stateful,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use tgbooks_core::{
    CatalogError, CatalogOperation, CatalogState, CoreCommand, DownloadKey, TransferState,
};

use super::{components::button, icons::Icon, is_activation_key, theme::Theme};

pub struct CatalogView {
    commands: Sender<CoreCommand>,
    state: CatalogState,
    pending: bool,
    choosing_directory: bool,
    download_pending: bool,
    transfer: TransferState,
    local_error: Option<&'static str>,
    channel_cursor: usize,
    book_cursor: usize,
    selected_books: HashSet<DownloadKey>,
    channel_focus: FocusHandle,
    book_focus: FocusHandle,
    detail_focus: FocusHandle,
    focus_after_selection: bool,
    channel_scroll: UniformListScrollHandle,
    book_scroll: UniformListScrollHandle,
}

impl CatalogView {
    pub fn new(commands: Sender<CoreCommand>, cx: &mut Context<Self>) -> Self {
        Self {
            commands,
            state: CatalogState::default(),
            pending: false,
            choosing_directory: false,
            download_pending: false,
            transfer: TransferState::default(),
            local_error: None,
            channel_cursor: 0,
            book_cursor: 0,
            selected_books: HashSet::new(),
            channel_focus: cx.focus_handle().tab_stop(true),
            book_focus: cx.focus_handle().tab_stop(true),
            detail_focus: cx.focus_handle(),
            focus_after_selection: false,
            channel_scroll: UniformListScrollHandle::new(),
            book_scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn set_state(&mut self, state: CatalogState, cx: &mut Context<Self>) {
        let cursor_id = self
            .state
            .channels
            .get(self.channel_cursor)
            .map(|channel| channel.id);
        self.channel_cursor = cursor_id
            .and_then(|id| state.channels.iter().position(|channel| channel.id == id))
            .unwrap_or(0);
        if state.selected != self.state.selected || state.books.is_empty() {
            self.book_cursor = 0;
            self.selected_books.clear();
            self.book_scroll = UniformListScrollHandle::new();
        }
        self.selected_books
            .retain(|key| state.books.iter().any(|book| book.key == *key));
        self.focus_after_selection |= state.selected != self.state.selected;
        if !state.authenticated || state.channels.is_empty() {
            self.focus_after_selection = false;
        }
        self.state = state;
        self.pending = false;
        self.local_error = None;
        cx.notify();
    }

    pub fn busy(&self) -> bool {
        self.pending || self.state.busy()
    }

    pub fn set_transfer(&mut self, state: TransferState, cx: &mut Context<Self>) {
        self.transfer = state;
        self.download_pending = false;
        cx.notify();
    }

    fn submit_download(
        &mut self,
        keys: Vec<DownloadKey>,
        directory: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.download_pending {
            return;
        }
        match self
            .commands
            .try_send(CoreCommand::DownloadBooks { keys, directory })
        {
            Ok(()) => self.download_pending = true,
            Err(_) => {
                self.local_error =
                    Some("Не удалось начать загрузку. Перезапустите приложение и повторите.")
            }
        }
        cx.notify();
    }

    fn choose_download(&mut self, cx: &mut Context<Self>) {
        if self.busy() || self.choosing_directory || self.download_pending {
            return;
        }
        let Some(book) = self.state.books.get(self.book_cursor) else {
            return;
        };
        let keys = if self.selected_books.is_empty() {
            vec![book.key]
        } else {
            // Catalogue order, not HashSet iteration order, defines FIFO order.
            self.state
                .books
                .iter()
                .filter(|book| self.selected_books.contains(&book.key))
                .map(|book| book.key)
                .collect()
        };
        self.choosing_directory = true;
        self.local_error = None;
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Выберите папку для выбранных книг".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = paths.await;
            let _ = view.update(cx, |view, cx| {
                view.choosing_directory = false;
                match result {
                    Ok(Ok(Some(mut paths))) if paths.len() == 1 => {
                        if let Some(directory) = paths.pop() {
                            view.submit_download(keys, directory, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => view.local_error = Some("Не удалось выбрать папку. Повторите действие."),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_book(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(book) = self.state.books.get(index)
            && !self.selected_books.insert(book.key)
        {
            self.selected_books.remove(&book.key);
        }
        cx.notify();
    }

    fn toggle_all_books(&mut self, cx: &mut Context<Self>) {
        if self.selected_books.len() == self.state.books.len() {
            self.selected_books.clear();
        } else {
            self.selected_books = self.state.books.iter().map(|book| book.key).collect();
        }
        cx.notify();
    }

    fn send(&mut self, command: CoreCommand, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        match self.commands.try_send(command) {
            Ok(()) => {
                self.pending = true;
                self.local_error = None;
            }
            Err(_) => {
                self.focus_after_selection = false;
                self.local_error =
                    Some("Сервис недоступен. Повторите позже или перезапустите приложение.");
            }
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.state.authenticated && self.transfer.active.is_none() {
            self.send(CoreCommand::RefreshChannels, cx);
        }
    }

    fn select(&mut self, id: Option<i64>, cx: &mut Context<Self>) {
        if !self.busy() {
            self.focus_after_selection = self.state.selected != id;
            self.send(CoreCommand::SelectChannel(id), cx);
        }
    }

    fn choose_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(channel) = self.state.channels.get(self.channel_cursor) {
            self.select(Some(channel.id), cx);
        }
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.state.can_scan() && self.transfer.active.is_none() {
            self.send(CoreCommand::ScanNextPage, cx);
        }
    }

    fn channel_list(&self, cx: &Context<Self>) -> Div {
        let theme = Theme::dark();
        div()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .gap_3()
            .child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(theme.muted)
                    .child(format!("Доступно каналов: {}", self.state.channels.len())),
            )
            .child(
                div()
                    .id("channel-list")
                    .role(Role::ListBox)
                    .aria_label("Каналы. Стрелки для навигации, Enter для выбора")
                    .track_focus(&self.channel_focus)
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(theme.border)
                    .rounded_sm()
                    .overflow_hidden()
                    .focus(move |style| style.border_color(theme.accent))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) {
                            view.choose_cursor(cx);
                            cx.stop_propagation();
                        } else if let Some(index) = moved_cursor(
                            &event.keystroke.key,
                            view.channel_cursor,
                            view.state.channels.len(),
                        ) {
                            view.channel_cursor = index;
                            view.channel_scroll
                                .scroll_to_item(index, ScrollStrategy::Nearest);
                            cx.notify();
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        uniform_list(
                            "channel-rows",
                            self.state.channels.len(),
                            cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                                range
                                    .map(|index| {
                                        let channel = &view.state.channels[index];
                                        let id = channel.id;
                                        let selected = view.state.selected == Some(id);
                                        let cursor = view.channel_cursor == index;
                                        let metadata = channel_metadata(channel);
                                        div()
                                            .id(("channel", id as u64))
                                            .role(Role::ListBoxOption)
                                            .aria_label(format!("{}. {metadata}", channel.title))
                                            .aria_selected(selected)
                                            .aria_position_in_set(index + 1)
                                            .aria_size_of_set(view.state.channels.len())
                                            .when(cursor, |row| row.aria_active_descendant())
                                            .h(px(64.))
                                            .w_full()
                                            .px_3()
                                            .flex()
                                            .flex_col()
                                            .justify_center()
                                            .gap_1()
                                            .border_1()
                                            .border_color(theme.canvas)
                                            .when(selected, |row| row.bg(theme.selected))
                                            .when(
                                                cursor && view.channel_focus.is_focused(window),
                                                |row| row.border_color(theme.accent),
                                            )
                                            .cursor_pointer()
                                            .hover(move |style| style.bg(theme.selected))
                                            .child(div().truncate().child(channel.title.clone()))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(theme.muted)
                                                    .truncate()
                                                    .child(metadata),
                                            )
                                            .on_click(cx.listener(move |view, _, window, cx| {
                                                view.channel_focus.focus(window, cx);
                                                view.channel_cursor = index;
                                                view.select(Some(id), cx);
                                            }))
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.channel_scroll)
                        .h_full(),
                    ),
            )
    }

    fn book_list(&self, cx: &Context<Self>) -> Stateful<Div> {
        let theme = Theme::dark();
        div()
            .id("book-results")
            .role(Role::ListBox)
            .aria_label("Найденные книги. Стрелки перемещают курсор, пробел отмечает книгу, Enter добавляет выбранные в очередь")
            .track_focus(&self.book_focus)
            .flex_1()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded_sm()
            .overflow_hidden()
            .focus(move |style| style.border_color(theme.accent))
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "space" {
                    view.toggle_book(view.book_cursor, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" {
                    view.choose_download(cx);
                    cx.stop_propagation();
                } else if let Some(index) = moved_cursor(
                    &event.keystroke.key,
                    view.book_cursor,
                    view.state.books.len(),
                ) {
                    view.book_cursor = index;
                    view.book_scroll
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(
                uniform_list(
                    "book-rows",
                    self.state.books.len(),
                    cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                        range
                            .map(|index| {
                                let book = &view.state.books[index];
                                let cursor = view.book_cursor == index;
                                let selected = view.selected_books.contains(&book.key);
                                let metadata = format!(
                                    "{} · {} · сообщение {}",
                                    book.format.extension().to_uppercase(),
                                    file_size(book.size_bytes),
                                    book.key.message_id,
                                );
                                div()
                                    .id(("book", index))
                                    .role(Role::ListBoxOption)
                                    .aria_label(format!("{}. {metadata}", book.file_name))
                                    .aria_selected(selected)
                                    .aria_position_in_set(index + 1)
                                    .aria_size_of_set(view.state.books.len())
                                    .when(cursor, |row| row.aria_active_descendant())
                                    .h(px(64.))
                                    .w_full()
                                    .px_3()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .gap_1()
                                    .border_1()
                                    .border_color(theme.canvas)
                                    .when(selected || cursor, |row| row.bg(theme.selected))
                                    .when(cursor && view.book_focus.is_focused(window), |row| {
                                        row.border_color(theme.accent)
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div().size(px(18.)).flex_shrink_0()
                                                    .border_1().border_color(if selected { theme.accent } else { theme.muted })
                                                    .rounded_sm()
                                                    .when(selected, |mark| mark.child(Icon::Check.element())),
                                            )
                                            .child(div().min_w_0().truncate().text_ellipsis_middle().child(book.file_name.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme.muted)
                                            .truncate()
                                            .child(metadata),
                                    )
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.book_focus.focus(window, cx);
                                        view.book_cursor = index;
                                        view.toggle_book(index, cx);
                                    }))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.book_scroll)
                .h_full(),
            )
    }

    fn details(&self, compact: bool, cx: &Context<Self>) -> Stateful<Div> {
        let theme = Theme::dark();
        let mut panel = div()
            .id("channel-details")
            .track_focus(&self.detail_focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .gap_3();
        let Some(channel) = self.state.selected_channel() else {
            return panel.child(
                div()
                    .pt_4()
                    .text_color(theme.muted)
                    .child("Выберите канал слева, чтобы найти книги в его истории."),
            );
        };
        let mut header = div().flex().items_center().flex_shrink_0().gap_3().child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .id("channel-title")
                        .role(Role::Heading)
                        .aria_level(2)
                        .aria_label(channel.title.clone())
                        .text_lg()
                        .truncate()
                        .child(channel.title.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted)
                        .truncate()
                        .child(channel_metadata(channel)),
                ),
        );
        if compact && !self.busy() {
            header = header.child(
                button("all-channels", "Все каналы")
                    .flex_shrink_0()
                    .text_sm()
                    .on_click(cx.listener(|view, _, _, cx| view.select(None, cx)))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) {
                            view.select(None, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }
        panel = panel.child(header);
        if channel.protected {
            return panel.child(
                div().text_color(theme.muted)
                    .child("Владелец канала запретил сохранение контента. Поиск и скачивание недоступны; приложение не обходит это ограничение."),
            );
        }
        if self.busy() {
            panel = panel.child(
                div()
                    .min_h(px(34.))
                    .flex_shrink_0()
                    .text_color(theme.muted)
                    .child(
                        if self.state.operation == Some(CatalogOperation::ListChannels) {
                            "Обновляем каналы…"
                        } else {
                            "Читаем сообщения…"
                        },
                    ),
            );
        } else if self.state.can_scan() && self.transfer.active.is_none() {
            panel = panel.child(
                button(
                    "scan-books",
                    if self.state.scan_started {
                        "Продолжить поиск"
                    } else {
                        "Найти книги"
                    },
                )
                .flex_shrink_0()
                .on_click(cx.listener(|view, _, _, cx| view.scan(cx)))
                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                    if is_activation_key(event) {
                        view.scan(cx);
                        cx.stop_propagation();
                    }
                })),
            );
        }
        panel = panel.child(
            div()
                .flex_shrink_0()
                .text_sm()
                .text_color(theme.muted)
                .child(format!(
                    "Сообщений: {}   Книг: {}   Пропущено защищённых: {}",
                    self.state.scanned_messages,
                    self.state.books.len(),
                    self.state.skipped_protected,
                )),
        );
        if !self.state.books.is_empty() {
            panel = panel
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .gap_3()
                        .child(
                            button(
                                "select-all-books",
                                if self.selected_books.len() == self.state.books.len() {
                                    "Снять выбор"
                                } else {
                                    "Выбрать все"
                                },
                            )
                            .text_sm()
                            .on_click(cx.listener(|view, _, _, cx| view.toggle_all_books(cx)))
                            .on_key_down(cx.listener(
                                |view, event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        view.toggle_all_books(cx);
                                        cx.stop_propagation();
                                    }
                                },
                            )),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted)
                                .child(format!("Выбрано: {}", self.selected_books.len())),
                        ),
                )
                .child(self.book_list(cx));
        } else {
            panel = panel.child(
                div().flex_1().min_h_0().text_color(theme.muted).child(
                    if !self.state.scan_started {
                        "Поиск проверяет по 100 сообщений, начиная с новых. Поддерживаются PDF, EPUB, FB2, MOBI, DJVU и ZIP."
                    } else if self.state.next_offset.is_some() {
                        "В этой части истории книг нет. Продолжите поиск в более старых сообщениях."
                    } else {
                        "История просмотрена. Подходящих вложений не найдено."
                    },
                ),
            );
        }
        if self.busy() || self.download_pending || self.choosing_directory {
            panel.child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(theme.muted)
                    .child(if self.choosing_directory {
                        "Выберите папку в системном диалоге…"
                    } else {
                        "Добавляем книги в очередь…"
                    }),
            )
        } else if !self.state.books.is_empty() {
            panel.child(
                button("download-selected-book", "Добавить выбранные в очередь…")
                    .flex_shrink_0()
                    .on_click(cx.listener(|view, _, _, cx| view.choose_download(cx)))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) {
                            view.choose_download(cx);
                            cx.stop_propagation();
                        }
                    })),
            )
        } else {
            panel
        }
    }
}

impl Render for CatalogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let compact = window.viewport_size().width < px(1000.);
        let hidden_list_focused =
            compact && self.state.selected.is_some() && self.channel_focus.is_focused(window);
        let hidden_books_focused =
            self.state.books.is_empty() && self.book_focus.is_focused(window);
        if !self.busy()
            && (self.focus_after_selection || hidden_list_focused || hidden_books_focused)
        {
            if compact && self.state.selected.is_some() {
                self.detail_focus.focus(window, cx);
            } else {
                self.channel_focus.focus(window, cx);
            }
            self.focus_after_selection = false;
        }
        let mut root = div().flex().flex_col().h_full().min_h_0().gap_3();
        if let Some(error) = self.local_error.map(str::to_owned).or_else(|| {
            self.state
                .error
                .map(|error| error_message(error, self.state.authenticated))
        }) {
            root = root.child(
                div()
                    .id("catalog-error")
                    .role(Role::Alert)
                    .aria_label(error.clone())
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(theme.error)
                    .child(error),
            );
        }
        if !self.state.authenticated {
            return root.child(
                div()
                    .text_color(theme.muted)
                    .child("Подключите Telegram в разделе «Вход», чтобы увидеть доступные каналы."),
            );
        }
        if self.state.channels.is_empty() {
            return root.child(div().text_color(theme.muted).child(
                if self.busy() { "Получаем каналы вашего аккаунта…" }
                else if self.state.error.is_some() { "Нажмите «Обновить», чтобы повторить получение каналов." }
                else { "Каналов пока нет. Здесь появятся каналы из ваших диалогов Telegram. Группы и личные чаты не включаются." },
            ));
        }
        if compact {
            root.child(if self.state.selected.is_some() {
                self.details(true, cx).into_any_element()
            } else {
                self.channel_list(cx).into_any_element()
            })
        } else {
            root.child(
                div()
                    .flex()
                    .h_full()
                    .min_h_0()
                    .gap_5()
                    .child(
                        div()
                            .w(px(240.))
                            .flex_shrink_0()
                            .h_full()
                            .child(self.channel_list(cx)),
                    )
                    .child(self.details(false, cx)),
            )
        }
    }
}

fn channel_metadata(channel: &tgbooks_core::ChannelSummary) -> String {
    let name = channel
        .username
        .as_ref()
        .map_or_else(|| "Без @имени".into(), |name| format!("@{name}"));
    if channel.protected {
        format!("Контент защищён · {name}")
    } else {
        name
    }
}

fn moved_cursor(key: &str, current: usize, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match key {
        "up" => Some(current.saturating_sub(1)),
        "down" => Some(current.saturating_add(1).min(count - 1)),
        "home" => Some(0),
        "end" => Some(count - 1),
        _ => None,
    }
}

pub(super) fn file_size(size: Option<u64>) -> String {
    match size {
        None => "Размер неизвестен".into(),
        Some(size) if size < 1024 => format!("{size} Б"),
        Some(size) if size < 1024 * 1024 => format!("{:.1} КБ", size as f64 / 1024.),
        Some(size) if size < 1024 * 1024 * 1024 => {
            format!("{:.1} МБ", size as f64 / (1024. * 1024.))
        }
        Some(size) => format!("{:.1} ГБ", size as f64 / (1024. * 1024. * 1024.)),
    }
}

fn error_message(error: CatalogError, authenticated: bool) -> String {
    match error {
        CatalogError::Busy => "Дождитесь окончания загрузки или отмените её в разделе «Загрузки».".into(),
        CatalogError::Unauthorized if authenticated => "Соединение восстановлено. Обновите список каналов.".into(),
        CatalogError::Unauthorized => "Сессия недействительна. Подключите Telegram заново в разделе «Вход».".into(),
        CatalogError::Unavailable => "Канал больше не доступен или Telegram отклонил запрос. Обновите список каналов.".into(),
        CatalogError::ProtectedContent => "Telegram запретил сохранение контента из этого канала.".into(),
        CatalogError::Network => "Не удалось связаться с Telegram. Проверьте сеть и повторите действие; результаты сохранены до закрытия приложения.".into(),
        CatalogError::Storage => "Не удалось сохранить зашифрованную сессию. Проверьте доступ к локальным данным и системному хранилищу ключей.".into(),
        CatalogError::FloodWait(seconds) => format!("Telegram ограничил запросы. Повторите действие не раньше чем через {seconds} с. Автоматических повторов нет."),
        CatalogError::InvalidResponse | CatalogError::UnsupportedReply => "Ответ Telegram не поддерживается. Список не изменён; повторите действие позже.".into(),
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use gpui::{TestAppContext, VisualTestContext, size};
    use tgbooks_core::ChannelSummary;

    use super::*;

    fn state(count: usize) -> CatalogState {
        CatalogState {
            authenticated: true,
            channels: (0..count)
                .map(|index| ChannelSummary {
                    id: -1000000000001 - index as i64,
                    title: format!("Канал {index}"),
                    username: None,
                    protected: false,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[gpui::test]
    fn keyboard_can_select_the_last_of_many_channels_without_duplicate_commands(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = CatalogView::new(ui.commands, cx);
            view.set_state(state(1000), cx);
            view.channel_focus.focus(window, cx);
            view
        });
        cx.simulate_keystrokes("end enter enter");
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SelectChannel(Some(-1000000001000)))
        ));
        assert!(backend.commands.try_recv().is_err());
        view.read_with(&cx.cx, |view, _| {
            assert_eq!(view.channel_cursor, 999);
            assert!(view.channel_scroll.0.borrow().base_handle.offset().y < px(0.));
            assert!(view.pending);
        });
        view.update(&mut cx.cx, |view, cx| {
            let mut selected = state(1000);
            selected.selected = Some(-1000000001000);
            view.set_state(selected, cx);
            view.scan(cx);
            view.scan(cx);
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::ScanNextPage)
        ));
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn directory_prompt_submits_one_typed_download_and_cancellation_submits_nothing(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| CatalogView::new(ui.commands, cx));
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 7,
            document_id: 8,
        };
        view.update(cx, |view, cx| {
            let mut selected = state(1);
            selected.selected = Some(key.channel_id);
            selected.books.push(tgbooks_core::BookAttachment {
                key,
                file_name: "book.epub".into(),
                size_bytes: Some(3),
                format: tgbooks_core::BookFormat::Epub,
            });
            view.set_state(selected, cx);
            view.choose_download(cx);
            view.choose_download(cx);
        });
        assert!(cx.did_prompt_for_paths());
        let folder = std::env::temp_dir().join("Книги");
        cx.simulate_path_prompt_response(|options| {
            assert!(options.directories);
            assert!(!options.files);
            assert!(!options.multiple);
            Some(vec![folder.clone()])
        });
        cx.run_until_parked();
        let Ok(CoreCommand::DownloadBooks {
            keys: actual,
            directory,
        }) = backend.commands.try_recv()
        else {
            panic!("native path selection did not send a download")
        };
        assert_eq!(actual, vec![key]);
        assert_eq!(directory, folder);
        assert!(backend.commands.try_recv().is_err());
        view.update(cx, |view, cx| {
            view.set_transfer(TransferState::default(), cx);
            view.choose_download(cx);
        });
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        assert!(!cx.did_prompt_for_paths());
        assert!(backend.commands.try_recv().is_err());
        view.read_with(cx, |view, _| {
            assert!(!view.choosing_directory);
            assert!(!view.download_pending);
        });
    }

    #[gpui::test]
    fn protected_or_exhausted_history_has_no_scan_action(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| CatalogView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            let mut selected = state(1);
            selected.selected = Some(selected.channels[0].id);
            selected.channels[0].protected = true;
            view.set_state(selected.clone(), cx);
            view.scan(cx);
            selected.channels[0].protected = false;
            selected.scan_started = true;
            view.set_state(selected, cx);
            view.scan(cx);
        });
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn compact_selection_and_back_navigation_keep_keyboard_focus_reachable(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let window = cx.open_window(size(px(440.), px(300.)), |window, cx| {
            let mut view = CatalogView::new(ui.commands, cx);
            view.set_state(state(2), cx);
            view.channel_focus.focus(window, cx);
            view
        });
        let Ok(view) = window.root(cx) else {
            panic!("catalog window missing")
        };
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.simulate_keystrokes("down enter");
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SelectChannel(Some(-1000000000002)))
        ));
        view.update(&mut visual.cx, |view, cx| {
            let mut selected = state(2);
            selected.selected = Some(-1000000000002);
            view.set_state(selected, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| assert!(view.read(cx).detail_focus.is_focused(window)));
        // The standalone view has no application's FocusNext action handler.
        visual.update(|window, cx| window.focus_next(cx));
        visual.simulate_keystrokes("enter");
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SelectChannel(None))
        ));
        view.update(&mut visual.cx, |view, cx| view.set_state(state(2), cx));
        visual.run_until_parked();
        visual.update(|window, cx| assert!(view.read(cx).channel_focus.is_focused(window)));
    }

    #[gpui::test]
    fn keyboard_selects_multiple_books_and_enqueues_them_in_catalogue_order_while_downloading(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let (view, cx) = cx.add_window_view(|_, cx| CatalogView::new(ui.commands, cx));
        let keys: Vec<_> = (0..3)
            .map(|index| DownloadKey {
                channel_id: -1000000000001,
                message_id: index + 1,
                document_id: i64::from(index + 10),
            })
            .collect();
        view.update(&mut cx.cx, |view, cx| {
            let mut state = state(1);
            state.selected = Some(keys[0].channel_id);
            state.books = keys
                .iter()
                .map(|&key| tgbooks_core::BookAttachment {
                    key,
                    file_name: "book.epub".into(),
                    size_bytes: Some(3),
                    format: tgbooks_core::BookFormat::Epub,
                })
                .collect();
            view.set_state(state, cx);
            view.set_transfer(
                TransferState {
                    active: Some(keys[2]),
                    ..Default::default()
                },
                cx,
            );
            view.focus_after_selection = false;
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.activate_window();
            view.read(cx).book_focus.clone().focus(window, cx);
        });
        cx.simulate_keystrokes("down space up space enter");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|_| Some(vec![std::env::temp_dir()]));
        cx.run_until_parked();
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::DownloadBooks { keys: actual, .. }) if actual == keys[..2])
        );
        assert!(backend.commands.try_recv().is_err());
        view.update(&mut cx.cx, |view, cx| {
            view.toggle_all_books(cx);
            assert_eq!(view.selected_books.len(), 3);
            view.toggle_all_books(cx);
            assert!(view.selected_books.is_empty());
            view.toggle_book(0, cx);
            let mut another = view.state.clone();
            another.selected = None;
            another.books.clear();
            view.set_state(another, cx);
            assert!(view.selected_books.is_empty());
        });
    }

    #[test]
    fn file_sizes_and_empty_list_navigation_handle_extremes() {
        assert_eq!(file_size(None), "Размер неизвестен");
        assert_eq!(file_size(Some(0)), "0 Б");
        assert_eq!(file_size(Some(1024)), "1.0 КБ");
        assert!(!file_size(Some(u64::MAX)).is_empty());
        assert_eq!(moved_cursor("end", 0, 0), None);
        assert_eq!(moved_cursor("up", 0, 1), Some(0));
        assert_eq!(moved_cursor("down", 0, 1), Some(0));
    }
}
