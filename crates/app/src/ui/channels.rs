//! Channel and attachment views consume only Core's display models and commands.

use std::collections::{HashMap, HashSet};

use async_channel::Sender;
use gpui::{
    Context, Div, EventEmitter, FocusHandle, KeyDownEvent, Role, ScrollStrategy, Stateful,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use tgbooks_core::{
    CatalogError, CatalogOperation, CatalogState, CoreCommand, DownloadKey, TransferState,
};

use super::{
    components::{button, checkbox, format_badge, icon_button, primary_button},
    files,
    icons::Icon,
    input::TextInput,
    is_activation_key,
    select::Select,
    theme::Theme,
};

pub(super) struct OpenRecommendations;

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
    channel_query: gpui::Entity<TextInput>,
    book_query: gpui::Entity<TextInput>,
    format: gpui::Entity<Select>,
    status: gpui::Entity<Select>,
    source_filter: gpui::Entity<Select>,
    source_pending: bool,
    source_error: Option<tgbooks_core::SourceError>,
    completed: HashMap<DownloadKey, String>,
    _filters: Vec<gpui::Subscription>,
    download_directory: Option<std::path::PathBuf>,
    download_notice: Option<String>,
    unread_recommendations: usize,
}

impl EventEmitter<OpenRecommendations> for CatalogView {}

impl CatalogView {
    pub fn new(commands: Sender<CoreCommand>, cx: &mut Context<Self>) -> Self {
        let channel_query = cx.new(|cx| TextInput::search("Поиск каналов", "Найти канал…", cx));
        let book_query =
            cx.new(|cx| TextInput::search("Поиск найденных файлов", "Среди найденных файлов…", cx));
        let format = cx.new(Select::formats);
        let source_filter =
            cx.new(|cx| Select::new("", vec!["Все каналы".into(), "Мои источники".into()], cx));
        let status = cx.new(|cx| {
            Select::new(
                "Статус",
                vec![
                    "Все".into(),
                    "Новые".into(),
                    "В очереди".into(),
                    "Скачано".into(),
                ],
                cx,
            )
        });
        let filters = vec![
            cx.observe(&channel_query, |_, _, cx| cx.notify()),
            cx.observe(&book_query, |_, _, cx| cx.notify()),
            cx.observe(&format, |_, _, cx| cx.notify()),
            cx.observe(&status, |_, _, cx| cx.notify()),
            cx.observe(&source_filter, |view, _, cx| {
                view.channel_cursor = 0;
                view.channel_scroll = UniformListScrollHandle::new();
                cx.notify();
            }),
        ];
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
            channel_query,
            book_query,
            format,
            status,
            source_filter,
            source_pending: false,
            source_error: None,
            completed: HashMap::new(),
            _filters: filters,
            download_directory: None,
            download_notice: None,
            unread_recommendations: 0,
        }
    }

    pub fn set_recommendation_count(&mut self, count: usize, cx: &mut Context<Self>) {
        if self.unread_recommendations != count {
            self.unread_recommendations = count;
            cx.notify();
        }
    }

    fn header_actions(&self, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(
                button(
                    "new-books-tab",
                    format!("Новые книги ({})", self.unread_recommendations),
                )
                .debug_selector(|| "catalog-new-books".into())
                .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenRecommendations)))
                .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                    if is_activation_key(event) {
                        cx.emit(OpenRecommendations);
                        cx.stop_propagation();
                    }
                })),
            )
            .when(self.state.authenticated, |actions| {
                if self.busy() || self.transfer.active.is_some() || self.commands.is_closed() {
                    actions.child(div().text_sm().text_color(Theme::dark().muted).child(
                        if self.commands.is_closed() {
                            "Сервис остановлен"
                        } else if self.transfer.active.is_some() {
                            "Идёт загрузка…"
                        } else {
                            "Обновление…"
                        },
                    ))
                } else {
                    actions.child(
                        button("refresh-channels", "Обновить каналы")
                            .debug_selector(|| "catalog-refresh".into())
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx)))
                            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                                if is_activation_key(event) {
                                    view.refresh(cx);
                                    cx.stop_propagation();
                                }
                            })),
                    )
                }
            })
    }

    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_selection(&mut self, cx: &mut Context<Self>) {
        self.selected_books = self
            .state
            .books
            .iter()
            .take(2)
            .map(|book| book.key)
            .collect();
        cx.notify();
    }

    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_completed(&mut self, cx: &mut Context<Self>) {
        self.selected_books.clear();
        self.book_cursor = 0;
        cx.notify();
    }

    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_sources(&mut self, missing: bool, cx: &mut Context<Self>) {
        let unavailable = tgbooks_core::ChannelSummary {
            id: -1000000099999,
            title: "Архив книг — сохранённый недоступный источник".into(),
            username: None,
            protected: false,
        };
        self.state.sources = tgbooks_core::SavedSources {
            account: Some(1),
            loaded: true,
            channels: self
                .state
                .channels
                .iter()
                .take(3)
                .cloned()
                .chain([unavailable.clone()])
                .collect(),
            ..Default::default()
        };
        self.state.channels_loaded = true;
        self.state.selected = missing.then_some(unavailable.id);
        self.state.books.clear();
        self.source_filter.update(cx, |filter, cx| {
            filter.selected = 1;
            cx.notify();
        });
        cx.notify();
    }

    pub fn set_history(
        &mut self,
        records: &[tgbooks_core::DownloadRecord],
        cx: &mut Context<Self>,
    ) {
        self.completed.clear();
        for record in records
            .iter()
            .filter(|record| record.status == tgbooks_core::DownloadStatus::Completed)
        {
            // History is newest-first. Keep the latest successful local copy.
            self.completed
                .entry(record.key)
                .or_insert_with(|| record.local_path.clone());
        }
        cx.notify();
    }

    fn waiting(&self, key: DownloadKey) -> bool {
        self.transfer.active == Some(key)
            || self.transfer.queued.iter().any(|job| job.book.key == key)
    }

    fn visible_channels(&self, cx: &gpui::App) -> Vec<usize> {
        self.channel_rows(cx)
            .iter()
            .enumerate()
            .filter(|(_, channel)| {
                files::matches(
                    &channel.title,
                    channel.username.as_deref(),
                    self.channel_query.read(cx).text(),
                    None,
                )
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn channel_rows(&self, cx: &gpui::App) -> Vec<&tgbooks_core::ChannelSummary> {
        if self.source_filter.read(cx).selected == 1 {
            self.state.source_channels()
        } else {
            self.state.channels.iter().collect()
        }
    }

    fn visible_books(&self, cx: &gpui::App) -> Vec<usize> {
        self.state
            .books
            .iter()
            .enumerate()
            .filter(|(_, book)| {
                files::matches(
                    &book.file_name,
                    None,
                    self.book_query.read(cx).text(),
                    self.format.read(cx).format(),
                ) && match self.status.read(cx).selected {
                    1 => !self.completed.contains_key(&book.key) && !self.waiting(book.key),
                    2 => self.waiting(book.key),
                    3 => self.completed.contains_key(&book.key),
                    _ => true,
                }
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn set_state(&mut self, state: CatalogState, cx: &mut Context<Self>) {
        let cursor_id = self
            .channel_rows(cx)
            .get(self.channel_cursor)
            .map(|channel| channel.id);
        if state.sources.account != self.state.sources.account || !state.authenticated {
            self.source_pending = false;
            self.source_error = None;
        }
        if state.selected != self.state.selected || state.books.is_empty() {
            self.book_cursor = 0;
            self.selected_books.clear();
            self.book_scroll = UniformListScrollHandle::new();
            self.download_notice = None;
        }
        self.selected_books
            .retain(|key| state.books.iter().any(|book| book.key == *key));
        self.focus_after_selection |= state.selected != self.state.selected;
        if !state.authenticated || state.channels.is_empty() && state.sources.channels.is_empty() {
            self.focus_after_selection = false;
        }
        self.state = state;
        self.channel_cursor = cursor_id
            .and_then(|id| {
                self.channel_rows(cx)
                    .iter()
                    .position(|channel| channel.id == id)
            })
            .unwrap_or(0);
        self.pending = false;
        self.local_error = None;
        cx.notify();
    }

    pub fn busy(&self) -> bool {
        self.pending || self.state.busy()
    }

    pub fn set_download_directory(
        &mut self,
        directory: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.download_directory = Some(directory);
        cx.notify();
    }

    pub fn set_transfer(&mut self, state: TransferState, cx: &mut Context<Self>) {
        if self.download_pending {
            self.download_notice = state.error.map(super::downloads::error_message).or_else(|| {
                (state.skipped_duplicates > 0).then(|| format!(
                    "Уже скачаны или находятся в очереди: {}. Повторы пропущены. Для новой копии выберите одну книгу и нажмите «Скачать повторно…».",
                    state.skipped_duplicates
                ))
            });
        }
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
        self.download_notice = None;
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

    fn selected_keys(&self, cx: &gpui::App) -> Vec<DownloadKey> {
        if self.selected_books.is_empty() {
            let indices = self.visible_books(cx);
            let Some(index) = indices
                .iter()
                .find(|&&index| index == self.book_cursor)
                .or(indices.first())
            else {
                return Vec::new();
            };
            vec![self.state.books[*index].key]
        } else {
            // Catalogue order, not HashSet iteration order, defines FIFO order.
            self.state
                .books
                .iter()
                .filter(|book| self.selected_books.contains(&book.key))
                .map(|book| book.key)
                .collect()
        }
    }

    fn download_default(&mut self, cx: &mut Context<Self>) {
        if self.busy() || self.choosing_directory || self.download_pending {
            return;
        }
        if self.download_directory.is_none() {
            self.choose_download(cx);
            return;
        }
        let keys = self.selected_keys(cx);
        if keys.is_empty() {
            return;
        }
        self.download_notice = None;
        match self.commands.try_send(CoreCommand::DownloadToDefault(keys)) {
            Ok(()) => self.download_pending = true,
            Err(_) => {
                self.local_error =
                    Some("Не удалось добавить книги в очередь. Перезапустите приложение.")
            }
        }
        cx.notify();
    }

    fn choose_download(&mut self, cx: &mut Context<Self>) {
        if self.busy() || self.choosing_directory || self.download_pending {
            return;
        }
        let keys = self.selected_keys(cx);
        if keys.is_empty() {
            return;
        }
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

    fn completed_selection(&self, cx: &gpui::App) -> Option<(DownloadKey, String)> {
        let keys = self.selected_keys(cx);
        if keys.len() != 1 {
            return None;
        }
        let key = keys[0];
        self.completed.get(&key).map(|path| (key, path.clone()))
    }

    fn redownload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.download_pending || self.choosing_directory {
            return;
        }
        let Some((key, _)) = self.completed_selection(cx) else {
            return;
        };
        if self.waiting(key) {
            return;
        }
        self.choosing_directory = true;
        let answer = window.prompt(
            gpui::PromptLevel::Info,
            "Скачать книгу повторно?",
            Some("Будет создана новая локальная копия. Существующий файл не перезаписывается."),
            &[
                gpui::PromptButton::new("В папку по умолчанию"),
                gpui::PromptButton::new("Другая папка…"),
                gpui::PromptButton::cancel("Отмена"),
            ],
            cx,
        );
        cx.spawn(async move |view, cx| {
            let answer = answer.await;
            if matches!(answer, Ok(1)) {
                let paths = cx.update(|cx| {
                    cx.prompt_for_paths(gpui::PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: Some("Папка для повторного скачивания".into()),
                    })
                });
                let result = paths.await;
                let _ = view.update(cx, |view, cx| {
                    view.choosing_directory = false;
                    match result {
                        Ok(Ok(Some(mut paths))) if paths.len() == 1 => {
                            if let Some(directory) = paths.pop() {
                                view.submit_redownload(key, Some(directory), cx);
                            }
                        }
                        Ok(Ok(None)) => {}
                        _ => {
                            view.local_error = Some("Не удалось выбрать папку. Повторите действие.")
                        }
                    }
                    cx.notify();
                });
            } else {
                let _ = view.update(cx, |view, cx| {
                    view.choosing_directory = false;
                    if matches!(answer, Ok(0)) {
                        view.submit_redownload(key, None, cx);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn submit_redownload(
        &mut self,
        key: DownloadKey,
        directory: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.download_notice = None;
        match self
            .commands
            .try_send(CoreCommand::RedownloadBook { key, directory })
        {
            Ok(()) => self.download_pending = true,
            Err(_) => {
                self.local_error =
                    Some("Не удалось добавить книгу в очередь. Перезапустите приложение.")
            }
        }
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
        let keys: Vec<_> = self
            .visible_books(cx)
            .iter()
            .map(|&index| self.state.books[index].key)
            .collect();
        let all = keys.iter().all(|key| self.selected_books.contains(key));
        for key in keys {
            if all {
                self.selected_books.remove(&key);
            } else {
                self.selected_books.insert(key);
            }
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

    pub fn complete_source(
        &mut self,
        result: Result<(), tgbooks_core::SourceError>,
        cx: &mut Context<Self>,
    ) {
        self.source_pending = false;
        self.source_error = result.err();
        cx.notify();
    }

    fn set_source(&mut self, id: i64, enabled: bool, cx: &mut Context<Self>) {
        if self.source_pending || self.busy() || !self.state.sources.loaded {
            return;
        }
        self.source_error = None;
        match self
            .commands
            .try_send(CoreCommand::SetChannelSource { id, enabled })
        {
            Ok(()) => self.source_pending = true,
            Err(_) => {
                self.local_error =
                    Some("Не удалось отправить изменение источника. Перезапустите приложение.")
            }
        }
        cx.notify();
    }

    fn source_control(&self, id: i64, cx: &Context<Self>) -> Div {
        let saved = self.state.sources.contains(id);
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .flex_shrink_0();
        if self.source_pending || self.busy() || !self.state.sources.loaded {
            return row.child(div().text_sm().text_color(Theme::dark().muted).child(
                if self.source_pending {
                    "Сохраняем источник…"
                } else if self.state.sources.error.is_some() {
                    "Источники недоступны. Повторите загрузку."
                } else {
                    "Изменение источников пока недоступно…"
                },
            ));
        }
        row = row.child(
            button(
                "channel-source-toggle",
                if saved {
                    "Убрать из источников"
                } else {
                    "Добавить в источники"
                },
            )
            .debug_selector(|| "channel-source-toggle".into())
            .on_click(cx.listener(move |view, _, _, cx| view.set_source(id, !saved, cx)))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                if is_activation_key(event) {
                    view.set_source(id, !saved, cx);
                    cx.stop_propagation();
                }
            })),
        );
        row.child(
            div()
                .text_size(px(11.))
                .text_color(Theme::dark().muted)
                .child("Только сохранение выбора — без автозагрузки"),
        )
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.state.authenticated && self.transfer.active.is_none() {
            self.send(CoreCommand::RefreshChannels, cx);
        }
    }

    pub(super) fn select(&mut self, id: Option<i64>, cx: &mut Context<Self>) {
        if !self.busy() {
            self.focus_after_selection = self.state.selected != id;
            self.send(CoreCommand::SelectChannel(id), cx);
        }
    }

    fn choose_cursor(&mut self, cx: &mut Context<Self>) {
        let indices = self.visible_channels(cx);
        if let Some(&index) = indices
            .iter()
            .find(|&&index| index == self.channel_cursor)
            .or(indices.first())
        {
            self.channel_cursor = index;
            self.select(Some(self.channel_rows(cx)[index].id), cx);
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
                    .child("Каналы"),
            )
            .child(self.source_filter.clone())
            .child(self.channel_query.clone())
            .when(self.visible_channels(cx).is_empty(), |list| list.child(
                div().text_sm().text_color(theme.muted).child(
                    if !self.channel_query.read(cx).text().is_empty() { "По вашему запросу ничего не найдено." }
                    else if self.source_filter.read(cx).selected == 1 { "Источников пока нет. Выберите «Все каналы», откройте канал и добавьте его в источники." }
                    else { "Каналов пока нет. Обновите список каналов." }
                )
            ))
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
                            view.visible_channels(cx)
                                .iter()
                                .position(|&index| index == view.channel_cursor)
                                .unwrap_or(0),
                            view.visible_channels(cx).len(),
                        ) {
                            view.channel_cursor = view.visible_channels(cx)[index];
                            view.channel_scroll
                                .scroll_to_item(index, ScrollStrategy::Nearest);
                            cx.notify();
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        uniform_list(
                            "channel-rows",
                            self.visible_channels(cx).len(),
                            cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                                let indices = view.visible_channels(cx);
                                let channels = view.channel_rows(cx);
                                range
                                    .map(|index| {
                                        let position = index;
                                        let index = indices[index];
                                        let channel = channels[index];
                                        let id = channel.id;
                                        let selected = view.state.selected == Some(id);
                                        let cursor = view.channel_cursor == index;
                                        let metadata = if !view.state.channels.iter().any(|live| live.id == id) {
                                            if view.state.channels_loaded { "Недоступен · нет в списке каналов".into() }
                                            else { "Доступность не проверена".into() }
                                        } else { channel_metadata(channel) };
                                        div()
                                            .id(("channel", id as u64))
                                            .role(Role::ListBoxOption)
                                            .aria_label(format!("{}. {metadata}{}", channel.title,
                                                if view.state.sources.contains(id) { ". Сохранённый источник" } else { "" }))
                                            .aria_selected(selected)
                                            .aria_position_in_set(position + 1)
                                            .aria_size_of_set(indices.len())
                                            .when(cursor, |row| row.aria_active_descendant())
                                            .h(px(58.))
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
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        if channel.protected {
                                                            Icon::Lock
                                                        } else {
                                                            Icon::Channels
                                                        }
                                                        .element(),
                                                    )
                                                    .child(
                                                        div()
                                                            .min_w_0()
                                                            .truncate()
                                                            .child(channel.title.clone()),
                                                    )
                                                    .when(view.state.sources.contains(id), |row| row.child(Icon::Check.element())),
                                            )
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
                    let indices = view.visible_books(cx);
                    if let Some(&index) = indices.iter().find(|&&index| index == view.book_cursor).or(indices.first()) {
                        view.book_cursor = index;
                        view.toggle_book(index, cx);
                    }
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" {
                    view.download_default(cx);
                    cx.stop_propagation();
                } else if let Some(index) = moved_cursor(
                    &event.keystroke.key,
                    view.visible_books(cx).iter().position(|&index| index == view.book_cursor).unwrap_or(0),
                    view.visible_books(cx).len(),
                ) {
                    view.book_cursor = view.visible_books(cx)[index];
                    view.book_scroll
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(
                uniform_list(
                    "book-rows",
                    self.visible_books(cx).len(),
                    cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                        let indices = view.visible_books(cx);
                        range
                            .map(|index| {
                                let position = index;
                                let index = indices[index];
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
                                    .aria_position_in_set(position + 1)
                                    .aria_size_of_set(indices.len())
                                    .when(cursor, |row| row.aria_active_descendant())
                                    .h(px(52.))
                                    .w_full()
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .border_1()
                                    .border_color(theme.canvas)
                                    .when(selected, |row| row.bg(theme.selection))
                                    .when(cursor && view.book_focus.is_focused(window), |row| {
                                        row.border_color(theme.accent)
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_4()
                                            .child(
                                                div().size(px(18.)).flex_shrink_0()
                                                    .border_1().border_color(if selected { theme.accent } else { theme.muted })
                                                    .rounded_sm()
                                                    .when(selected, |mark| mark.child(Icon::Check.element())),
                                            )
                                            .child(div().min_w_0().flex_1().flex().flex_col().gap_1()
                                                .child(div().truncate().text_ellipsis_middle().child(book.file_name.clone()))
                                                .when(view.waiting(book.key) || view.completed.contains_key(&book.key), |row| row.child(
                                                    div().text_size(px(11.)).text_color(theme.muted).child(
                                                        if view.completed.contains_key(&book.key) { "Скачано" } else { "В очереди" }
                                                    )))),
                                    )
                                    .child(format_badge(Some(book.format)))
                                    .child(div().w(px(80.)).flex_shrink_0().text_right().text_sm().text_color(theme.muted).child(file_size(book.size_bytes)))
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
            div().flex_1().min_w_0().child(
                div()
                    .id("channel-title")
                    .debug_selector(|| "channel-title".into())
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(channel.title.clone())
                    .text_lg()
                    .truncate()
                    .child(channel.title.clone()),
            ),
        );
        if compact && !self.busy() {
            header = header.child(
                button("all-channels", "К списку")
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
        panel = panel
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_shrink_0()
                    .child(header.child(self.header_actions(cx)))
                    .when(!compact, |header| {
                        header.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted)
                                .truncate()
                                .child(channel_metadata(channel)),
                        )
                    }),
            )
            .child(self.source_control(channel.id, cx));
        if !self.state.channels.iter().any(|live| live.id == channel.id) {
            return panel.child(div().text_sm().text_color(theme.muted).child(
                if self.state.channels_loaded {
                    "Канала нет в текущем списке доступных каналов. Источник сохранён; обновите каналы или уберите его из источников. Поиск и скачивание отсюда недоступны."
                } else {
                    "Не удалось проверить доступность этого источника. Обновите каналы. Сохранённый источник не удалён."
                }
            ));
        }
        if channel.protected {
            return panel.child(
                div().text_color(theme.muted)
                    .child("Владелец канала запретил сохранение контента. Поиск и скачивание недоступны; приложение не обходит это ограничение."),
            );
        }
        let scan_control = if self.busy() {
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
                )
                .into_any_element()
        } else if self.state.can_scan() && self.transfer.active.is_none() {
            button(
                "scan-books",
                if self.state.scan_started {
                    "Ещё 100 сообщений"
                } else {
                    "Повторить загрузку сообщений"
                },
            )
            .flex_shrink_0()
            .on_click(cx.listener(|view, _, _, cx| view.scan(cx)))
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if is_activation_key(event) {
                    view.scan(cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
        } else {
            div().into_any_element()
        };
        if !self.state.books.is_empty() {
            panel = panel
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .items_center()
                        .flex_shrink_0()
                        .child(div().flex_1().min_w_0().child(self.book_query.clone()))
                        .child(self.format.clone())
                        .child(self.status.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .gap_3()
                        .child(
                            checkbox(
                                "select-all-books",
                                "Выбрать или снять выбор видимых файлов",
                                !self.visible_books(cx).is_empty()
                                    && self.visible_books(cx).iter().all(|&index| {
                                        self.selected_books.contains(&self.state.books[index].key)
                                    }),
                                self.visible_books(cx).iter().any(|&index| {
                                    self.selected_books.contains(&self.state.books[index].key)
                                }) && !self.visible_books(cx).iter().all(|&index| {
                                    self.selected_books.contains(&self.state.books[index].key)
                                }),
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
                                .flex_1()
                                .child("Файл"),
                        )
                        .child(
                            div()
                                .w(px(50.))
                                .flex_shrink_0()
                                .text_size(px(12.))
                                .text_color(theme.muted)
                                .truncate()
                                .child("Формат"),
                        )
                        .child(
                            div()
                                .w(px(80.))
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(theme.muted)
                                .text_right()
                                .child("Размер"),
                        ),
                )
                .child(self.book_list(cx));
        } else {
            panel = panel.child(
                div().flex_1().min_h_0().text_color(theme.muted).child(
                    if !self.state.scan_started {
                        "Первые 100 сообщений загружаются при выборе канала. Поддерживаются PDF, EPUB, FB2, MOBI, DJVU и ZIP."
                    } else if self.state.next_offset.is_some() {
                        "В этой части истории книг нет. Продолжите поиск в более старых сообщениях."
                    } else {
                        "История просмотрена. Подходящих вложений не найдено."
                    },
                ),
            );
        }
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .flex_shrink_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .child(format!(
                            "Найдено {} файлов · Просмотрено {} сообщений{}",
                            self.state.books.len(),
                            self.state.scanned_messages,
                            if self.state.skipped_protected > 0 {
                                format!(" · Защищённых: {}", self.state.skipped_protected)
                            } else {
                                String::new()
                            }
                        )),
                )
                .child(scan_control),
        );
        panel
    }

    fn selection_bar(&self, compact: bool, cx: &Context<Self>) -> Div {
        let theme = Theme::dark();
        if !self.busy()
            && !self.download_pending
            && !self.choosing_directory
            && let Some((key, path)) = self.completed_selection(cx)
        {
            let key_path = path.clone();
            return div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .gap_2()
                .p_3()
                .bg(theme.selected)
                .rounded_md()
                .child(div().text_sm().child(if self.waiting(key) {
                    "Книга уже скачана; новая загрузка находится в очереди"
                } else {
                    "Книга уже скачана"
                }))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .truncate()
                        .text_ellipsis_middle()
                        .child(path.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            button("catalog-reveal", "Показать в папке")
                                .on_click(move |_, _, cx| crate::platform::reveal_file(&path, cx))
                                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        crate::platform::reveal_file(&key_path, cx);
                                        cx.stop_propagation();
                                    }
                                }),
                        )
                        .when(!self.waiting(key), |bar| {
                            bar.child(
                                button("catalog-redownload", "Скачать повторно…")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.redownload(window, cx)
                                    }))
                                    .on_key_down(cx.listener(
                                        |view, event: &KeyDownEvent, window, cx| {
                                            if is_activation_key(event) {
                                                view.redownload(window, cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    )),
                            )
                        }),
                );
        }
        let mut detail = Vec::new();
        if !self.selected_books.is_empty() {
            detail.push(file_size(
                self.state
                    .books
                    .iter()
                    .filter(|book| self.selected_books.contains(&book.key))
                    .try_fold(0_u64, |total, book| total.checked_add(book.size_bytes?)),
            ));
        }
        if let Some(directory) = &self.download_directory {
            detail.push(format!("Папка: {}", files::folder_path(directory)));
        }
        let detail = detail.join(" · ");
        if self.busy() || self.download_pending || self.choosing_directory {
            div()
                .flex_shrink_0()
                .text_sm()
                .text_color(theme.muted)
                .child(if self.choosing_directory {
                    "Выберите папку в системном диалоге…"
                } else if self.busy() {
                    "Читаем сообщения…"
                } else {
                    "Добавляем книги в очередь…"
                })
        } else if !self.state.books.is_empty() {
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_3()
                .p_3()
                .bg(theme.selected)
                .rounded_md()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_sm()
                        .child(div().truncate().child(if self.selected_books.is_empty() {
                            "Выберите файлы".into()
                        } else {
                            format!(
                                "Выбрано {}",
                                files::count(self.selected_books.len(), "файл", "файла", "файлов")
                            )
                        }))
                        .when(!detail.is_empty(), |label| {
                            label.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted)
                                    .truncate()
                                    .child(detail),
                            )
                        }),
                )
                .when(!self.selected_books.is_empty(), |bar| {
                    bar.child(
                        button("clear-book-selection", "Снять выбор")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.selected_books.clear();
                                cx.notify();
                            }))
                            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                                if is_activation_key(event) {
                                    view.selected_books.clear();
                                    cx.notify();
                                    cx.stop_propagation();
                                }
                            })),
                    )
                })
                .child(
                    (if compact {
                        icon_button(
                            "download-other-directory",
                            "Выбрать другую папку для этой группы",
                            Icon::Channels,
                        )
                    } else {
                        button("download-other-directory", "Другая папка…")
                    })
                    .on_click(cx.listener(|view, _, _, cx| view.choose_download(cx)))
                    .on_key_down(cx.listener(
                        |view, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) {
                                view.choose_download(cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
                )
                .child(
                    primary_button(
                        "download-selected-book",
                        if self.selected_books.is_empty() {
                            "Добавить файл…".into()
                        } else {
                            format!("Добавить {} в очередь…", self.selected_books.len())
                        },
                    )
                    .flex_shrink_0()
                    .on_click(cx.listener(|view, _, _, cx| view.download_default(cx)))
                    .on_key_down(cx.listener(
                        |view, event: &KeyDownEvent, _, cx| {
                            if is_activation_key(event) {
                                view.download_default(cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
                )
        } else {
            div()
        }
    }
}

impl Render for CatalogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let compact = window.viewport_size().width < px(1180.);
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
        if self.state.selected_channel().is_none() {
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_shrink_0()
                    .child(
                        div()
                            .id("channel-overview-title")
                            .debug_selector(|| "channel-overview-title".into())
                            .role(Role::Heading)
                            .aria_level(1)
                            .text_lg()
                            .child("Каналы"),
                    )
                    .child(self.header_actions(cx)),
            );
        }
        if let Some(error) = self.source_error.or(self.state.sources.error) {
            let message = match error {
                tgbooks_core::SourceError::Storage => {
                    "Не удалось загрузить или сохранить источники. Сохранённый выбор не изменён."
                }
                tgbooks_core::SourceError::Unauthorized => {
                    "Подключите Telegram, чтобы сохранить источники для аккаунта."
                }
                tgbooks_core::SourceError::Unavailable => {
                    "Канал больше не доступен. Обновите список каналов."
                }
            };
            root = root.child(
                div()
                    .id("sources-error")
                    .role(Role::Alert)
                    .aria_label(message)
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(theme.error)
                    .child(message)
                    .when(self.state.sources.error.is_some() && !self.busy(), |row| {
                        row.child(
                            button("reload-sources", "Повторить загрузку источников")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.send(CoreCommand::ReloadSources, cx)
                                }))
                                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        view.send(CoreCommand::ReloadSources, cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        )
                    }),
            );
        }
        if let Some(notice) = &self.download_notice {
            root = root.child(
                div()
                    .id("catalog-download-notice")
                    .role(Role::Status)
                    .aria_label(notice.clone())
                    .text_sm()
                    .text_color(theme.muted)
                    .flex_shrink_0()
                    .child(notice.clone()),
            );
        }
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
        if self.state.channels.is_empty()
            && self.state.sources.channels.is_empty()
            && !self.state.sources.loaded
        {
            return root.child(div().text_color(theme.muted).child(
                if self.busy() { "Получаем каналы вашего аккаунта…" }
                else if self.state.error.is_some() { "Нажмите «Обновить», чтобы повторить получение каналов." }
                else { "Каналов пока нет. Здесь появятся каналы из ваших диалогов Telegram. Группы и личные чаты не включаются." },
            ));
        }
        let layout = if compact {
            div()
                .flex_1()
                .min_h_0()
                .child(if self.state.selected.is_some() {
                    self.details(true, cx).into_any_element()
                } else {
                    self.channel_list(cx).into_any_element()
                })
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .gap_5()
                .child(
                    div()
                        .w(px(224.))
                        .flex_shrink_0()
                        .h_full()
                        .pr_3()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(self.channel_list(cx)),
                )
                .child(self.details(false, cx))
        };
        root.child(layout).when(
            self.state
                .selected_channel()
                .is_some_and(|channel| !channel.protected)
                && !self.state.books.is_empty(),
            |root| root.child(self.selection_bar(window.viewport_size().width < px(1000.), cx)),
        )
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

pub(super) fn moved_cursor(key: &str, current: usize, count: usize) -> Option<usize> {
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

pub(super) fn error_message(error: CatalogError, authenticated: bool) -> String {
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
    fn missing_source_can_be_removed_with_keyboard_in_a_small_window(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let id = -1000000000001;
        let window = cx.open_window(size(px(440.), px(300.)), |_, cx| {
            let mut view = CatalogView::new(ui.commands, cx);
            let mut saved = state(1);
            saved.sources = tgbooks_core::SavedSources {
                account: Some(1),
                loaded: true,
                channels: saved.channels.clone(),
                ..Default::default()
            };
            saved.channels.clear();
            saved.channels_loaded = true;
            saved.selected = Some(id);
            view.set_state(saved, cx);
            view
        });
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        let Some(bounds) = visual.debug_bounds("channel-source-toggle") else {
            panic!("source control missing");
        };
        assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(300.));
        // Back, new books and refresh precede source membership.
        visual.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
            window.focus_next(cx);
            window.focus_next(cx);
        });
        visual.simulate_keystrokes("enter");
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::SetChannelSource { id: actual, enabled: false }) if actual == id)
        );
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn sources_filter_includes_missing_channels_and_uses_live_metadata(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| CatalogView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            let mut value = state(2);
            let mut saved = value.channels[0].clone();
            saved.title = "Старое имя".into();
            value.channels[0].title = "Новое имя".into();
            let mut missing = saved.clone();
            missing.id = -1009999999999;
            missing.title = "Недоступный источник 世界".into();
            value.sources = tgbooks_core::SavedSources {
                account: Some(1),
                loaded: true,
                channels: vec![saved, missing],
                ..Default::default()
            };
            value.channels_loaded = true;
            view.set_state(value, cx);
            assert_eq!(view.channel_rows(cx).len(), 2);
            view.source_filter.update(cx, |filter, cx| {
                filter.selected = 1;
                cx.notify();
            });
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            let rows = view.channel_rows(cx);
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().any(|channel| channel.title == "Новое имя"));
            let missing = rows.iter().position(|channel| channel.id == -1009999999999);
            let Some(missing) = missing else {
                panic!("missing source disappeared");
            };
            view.channel_cursor = missing;
            view.choose_cursor(cx);
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SelectChannel(Some(-1009999999999)))
        ));
        assert!(
            backend.commands.try_recv().is_err(),
            "filtering must not start downloads"
        );
    }

    #[gpui::test]
    fn source_changes_wait_for_ack_and_keep_previous_selection_on_error(cx: &mut TestAppContext) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| CatalogView::new(ui.commands, cx));
        view.update(cx, |view, cx| {
            let mut value = state(1);
            let id = value.channels[0].id;
            value.sources.account = Some(1);
            value.sources.loaded = true;
            view.set_state(value.clone(), cx);
            view.set_source(id, true, cx);
            view.set_source(id, true, cx);
            assert!(!view.state.sources.contains(id));
            assert!(view.source_pending);
            view.set_state(value.clone(), cx);
            assert!(
                view.source_pending,
                "unrelated catalogue state is not a save acknowledgement"
            );
            view.complete_source(Err(tgbooks_core::SourceError::Storage), cx);
            assert!(!view.state.sources.contains(id));
            assert!(!view.source_pending);
            view.set_source(id, true, cx);
            value.sources.channels = value.channels.clone();
            view.set_state(value.clone(), cx);
            view.complete_source(Ok(()), cx);
            assert!(view.state.sources.contains(id));
            value.sources = tgbooks_core::SavedSources {
                account: Some(2),
                loaded: true,
                ..Default::default()
            };
            value.channels.clear();
            view.set_state(value, cx);
            assert!(view.channel_rows(cx).is_empty());
            assert!(view.source_error.is_none());
        });
        for _ in 0..2 {
            assert!(matches!(
                backend.commands.try_recv(),
                Ok(CoreCommand::SetChannelSource { enabled: true, .. })
            ));
        }
        assert!(backend.commands.try_recv().is_err());
    }

    #[gpui::test]
    fn duplicate_feedback_and_explicit_repeat_require_a_destination_choice(
        cx: &mut TestAppContext,
    ) {
        use tgbooks_core::{
            BookAttachment, BookFormat, DownloadProgress, DownloadRecord, DownloadStatus,
        };
        let (ui, backend) = tgbooks_core::connect();
        let (view, cx) = cx.add_window_view(|_, cx| CatalogView::new(ui.commands, cx));
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 1,
            document_id: 2,
        };
        let Ok(progress) = DownloadProgress::new(3, Some(3)) else {
            panic!("invalid fixture progress");
        };
        let original = DownloadRecord {
            key,
            file_name: "book.epub".into(),
            local_path: "/old/book.epub".into(),
            source_title: None,
            added_at: 0,
            status: DownloadStatus::Completed,
            progress,
        };
        let directory = std::env::temp_dir().join("default");
        view.update(&mut cx.cx, |view, cx| {
            let mut value = state(1);
            value.selected = Some(key.channel_id);
            value.books.push(BookAttachment {
                key,
                file_name: "book.epub".into(),
                format: BookFormat::Epub,
                size_bytes: Some(3),
            });
            view.set_state(value, cx);
            view.set_history(&[original], cx);
            view.set_download_directory(directory.clone(), cx);
            assert_eq!(
                view.completed_selection(cx),
                Some((key, "/old/book.epub".into()))
            );
            view.download_default(cx);
            view.set_transfer(
                TransferState {
                    skipped_duplicates: 1,
                    ..Default::default()
                },
                cx,
            );
            assert!(
                view.download_notice
                    .as_ref()
                    .is_some_and(|notice| notice.contains("Повторы пропущены"))
            );
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::DownloadToDefault(_))
        ));
        assert!(!cx.has_pending_prompt());
        cx.update(|window, cx| view.update(cx, |view, cx| view.redownload(window, cx)));
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Отмена");
        cx.run_until_parked();
        assert!(backend.commands.try_recv().is_err());

        cx.update(|window, cx| view.update(cx, |view, cx| view.redownload(window, cx)));
        cx.simulate_prompt_answer("В папку по умолчанию");
        cx.run_until_parked();
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::RedownloadBook { key: actual, directory: None }) if actual == key)
        );
        assert!(!cx.did_prompt_for_paths());
        view.update(&mut cx.cx, |view, cx| {
            view.set_transfer(TransferState::default(), cx)
        });

        cx.update(|window, cx| view.update(cx, |view, cx| view.redownload(window, cx)));
        cx.simulate_prompt_answer("Другая папка…");
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        assert!(backend.commands.try_recv().is_err());

        cx.update(|window, cx| view.update(cx, |view, cx| view.redownload(window, cx)));
        cx.simulate_prompt_answer("Другая папка…");
        cx.run_until_parked();
        let other = std::env::temp_dir().join("other");
        cx.simulate_path_prompt_response(|_| Some(vec![other.clone()]));
        cx.run_until_parked();
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::RedownloadBook { key: actual, directory: Some(path) }) if actual == key && path == other)
        );
        view.read_with(&cx.cx, |view, _| {
            assert_eq!(view.download_directory.as_ref(), Some(&directory))
        });
    }

    #[gpui::test]
    fn default_destination_needs_no_prompt_and_one_off_folder_does_not_change_it(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        let view = cx.new(|cx| CatalogView::new(ui.commands, cx));
        let directory = std::env::temp_dir().join("default");
        let key = DownloadKey {
            channel_id: -1000000000001,
            message_id: 1,
            document_id: 2,
        };
        view.update(cx, |view, cx| {
            let mut value = state(1);
            value.selected = Some(key.channel_id);
            value.books.push(tgbooks_core::BookAttachment {
                key,
                file_name: "book.epub".into(),
                format: tgbooks_core::BookFormat::Epub,
                size_bytes: Some(3),
            });
            view.set_state(value, cx);
            view.set_download_directory(directory.clone(), cx);
            view.download_default(cx);
            view.download_default(cx);
        });
        assert!(!cx.did_prompt_for_paths());
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::DownloadToDefault(keys)) if keys == vec![key])
        );
        assert!(backend.commands.try_recv().is_err());
        view.update(cx, |view, cx| {
            view.set_transfer(TransferState::default(), cx);
            view.choose_download(cx);
        });
        let other = std::env::temp_dir().join("one-off");
        cx.simulate_path_prompt_response(|_| Some(vec![other.clone()]));
        cx.run_until_parked();
        assert!(
            matches!(backend.commands.try_recv(), Ok(CoreCommand::DownloadBooks { directory: actual, .. }) if actual == other)
        );
        view.read_with(cx, |view, _| {
            assert_eq!(view.download_directory.as_ref(), Some(&directory))
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

    #[gpui::test]
    fn filtering_never_selects_hidden_channels_and_select_all_is_scoped_to_visible_files(
        cx: &mut TestAppContext,
    ) {
        use gpui::EntityInputHandler;
        let (ui, backend) = tgbooks_core::connect();
        let (view, cx) = cx.add_window_view(|_, cx| CatalogView::new(ui.commands, cx));
        view.update(&mut cx.cx, |view, cx| view.set_state(state(3), cx));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.channel_query.update(cx, |input, cx| {
                    input.replace_text_in_range(None, "Канал 2", window, cx)
                });
                view.choose_cursor(cx);
            })
        });
        assert!(matches!(
            backend.commands.try_recv(),
            Ok(CoreCommand::SelectChannel(Some(-1000000000003)))
        ));
        view.update(&mut cx.cx, |view, cx| {
            let mut state = state(3);
            state.selected = Some(state.channels[0].id);
            state.books = ["one.pdf", "two.epub", "three.pdf"]
                .iter()
                .enumerate()
                .map(|(index, name)| tgbooks_core::BookAttachment {
                    key: DownloadKey {
                        channel_id: state.channels[0].id,
                        message_id: index as i32 + 1,
                        document_id: index as i64 + 1,
                    },
                    file_name: (*name).into(),
                    size_bytes: Some(10),
                    format: tgbooks_core::BookFormat::from_file_name(name)
                        .unwrap_or(tgbooks_core::BookFormat::Pdf),
                })
                .collect();
            view.set_state(state, cx);
            view.format.update(cx, |select, cx| {
                select.selected = 1;
                cx.notify();
            });
            assert_eq!(view.visible_books(cx), vec![0, 2]);
            view.toggle_all_books(cx);
            assert_eq!(view.selected_books.len(), 2);
            assert!(!view.selected_books.contains(&view.state.books[1].key));
            view.toggle_all_books(cx);
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
