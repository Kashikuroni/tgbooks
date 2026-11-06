//! Local library/history tables. OS actions are delegated to GPUI's platform.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, Role, ScrollStrategy, Subscription,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use tgbooks_core::{BookFormat, DownloadKey, DownloadRecord, DownloadStatus};

use super::{
    channels::{file_size, moved_cursor},
    components::{button, empty_state, format_badge, icon_button, primary_button},
    files,
    icons::Icon,
    input::TextInput,
    is_activation_key,
    select::Select,
    theme::Theme,
};
use crate::platform;

pub struct GoToChannels;

pub struct FileTable {
    records: Vec<DownloadRecord>,
    history: bool,
    loading: bool,
    query: Entity<TextInput>,
    format: Entity<Select>,
    sort: Entity<Select>,
    selected: Option<(DownloadKey, String)>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    _observers: Vec<Subscription>,
}

impl EventEmitter<GoToChannels> for FileTable {}

impl FileTable {
    #[cfg(feature = "gpui-tests")]
    pub(super) fn preview_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = self.visible(cx).first().map(|&index| {
            (
                self.records[index].key,
                self.records[index].local_path.clone(),
            )
        });
        cx.notify();
    }

    pub fn new(history: bool, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::search("Поиск файлов", "Найти файл или источник…", cx));
        let format = cx.new(Select::formats);
        let sort = cx.new(|cx| {
            Select::new(
                "",
                vec![
                    "Сначала новые".into(),
                    "Сначала старые".into(),
                    "По названию".into(),
                ],
                cx,
            )
        });
        let observers = vec![
            cx.observe(&query, |view, _, cx| view.filter_changed(cx)),
            cx.observe(&format, |view, _, cx| view.filter_changed(cx)),
            cx.observe(&sort, |view, _, cx| view.filter_changed(cx)),
        ];
        Self {
            records: Vec::new(),
            history,
            loading: true,
            query,
            format,
            sort,
            selected: None,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            _observers: observers,
        }
    }

    pub fn set_records(
        &mut self,
        records: &[DownloadRecord],
        loading: bool,
        cx: &mut Context<Self>,
    ) {
        self.records = records
            .iter()
            .filter(|record| {
                record.status == DownloadStatus::Completed
                    || self.history && record.status == DownloadStatus::Failed
            })
            .cloned()
            .collect();
        self.loading = loading;
        self.filter_changed(cx);
    }

    fn visible(&self, cx: &gpui::App) -> Vec<usize> {
        let query = self.query.read(cx).text();
        let format = self.format.read(cx).format();
        let mut indices: Vec<_> = self
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| {
                files::matches(
                    &record.file_name,
                    record.source_title.as_deref(),
                    query,
                    format,
                )
            })
            .map(|(index, _)| index)
            .collect();
        indices.sort_by(|&a, &b| {
            let (a, b) = (&self.records[a], &self.records[b]);
            match self.sort.read(cx).selected {
                1 => a.added_at.cmp(&b.added_at),
                2 => a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()),
                _ => b.added_at.cmp(&a.added_at),
            }
            .then_with(|| a.key.channel_id.cmp(&b.key.channel_id))
            .then_with(|| a.key.message_id.cmp(&b.key.message_id))
        });
        indices
    }

    fn filter_changed(&mut self, cx: &mut Context<Self>) {
        if self.selected.as_ref().is_some_and(|(key, path)| {
            !self.visible(cx).iter().any(|&index| {
                self.records[index].key == *key && self.records[index].local_path == *path
            })
        }) {
            self.selected = None;
        }
        cx.notify();
    }

    fn selected_record(&self) -> Option<&DownloadRecord> {
        self.records.iter().find(|record| {
            self.selected
                .as_ref()
                .is_some_and(|(key, path)| record.key == *key && record.local_path == *path)
        })
    }

    fn activate(&self, cx: &mut Context<Self>) {
        if let Some(record) = self
            .selected_record()
            .filter(|record| record.status == DownloadStatus::Completed)
        {
            platform::open_file(&record.local_path, cx);
        }
    }

    fn move_selection(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let indices = self.visible(cx);
        if indices.is_empty() {
            return false;
        }
        let current = indices.iter().position(|&index| {
            self.selected.as_ref().is_some_and(|(key, path)| {
                self.records[index].key == *key && self.records[index].local_path == *path
            })
        });
        let next = match (current, key) {
            (None, "down" | "home") => Some(0),
            (None, "up" | "end") => Some(indices.len() - 1),
            _ => moved_cursor(key, current.unwrap_or(0), indices.len()),
        };
        let Some(next) = next else {
            return false;
        };
        self.selected = Some((
            self.records[indices[next]].key,
            self.records[indices[next]].local_path.clone(),
        ));
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
        true
    }
}

impl Render for FileTable {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let indices = self.visible(cx);
        let count = indices.len();
        let mut root = div().flex().flex_col().h_full().min_h_0().gap_4().child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .flex_shrink_0()
                .child(div().flex_1().min_w_0().child(self.query.clone()))
                .child(self.format.clone())
                .child(self.sort.clone()),
        );
        if self.records.is_empty() {
            return root.child(div().flex_1().flex().flex_col().justify_center().items_center()
                .child(empty_state(
                    if self.loading { "Читаем локальную историю…" }
                    else if self.history { "История пока пуста" } else { "В библиотеке пока нет файлов" },
                    "Найдите книги в каналах и добавьте их в очередь. Скачанные файлы останутся на этом устройстве.",
                ))
                .child(primary_button("library-go-channels", "Перейти к каналам")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(GoToChannels)))
                    .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                        if is_activation_key(event) { cx.emit(GoToChannels); cx.stop_propagation(); }
                    }))));
        }
        let compact = window.viewport_size().width < px(1100.);
        root = root.child(div().flex().items_center().px_3().gap_4().h(px(24.)).text_sm().text_color(theme.muted)
            .child(div().w(px(18.)))
            .child(div().flex_1().child("Название и источник"))
            .child(div().w(px(50.)).flex_shrink_0().text_size(px(12.)).truncate().child("Формат"))
            .child(div().w(px(76.)).flex_shrink_0().text_right().child("Размер"))
            .when(!compact, |row| row.child(div().w(px(142.)).flex_shrink_0().text_right().child("Добавлено")))
            .when(self.history, |row| row.child(div().w(px(100.)).flex_shrink_0().child("Статус"))))
            .child(
                div().id("file-table").role(Role::ListBox).aria_label(if self.history { "История загрузок" } else { "Скачанные файлы" })
                    .track_focus(&self.focus).flex_1().min_h_0().overflow_hidden()
                    .border_1().border_color(theme.canvas).focus(move |style| style.border_color(theme.accent))
                    .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "enter" { view.activate(cx); cx.stop_propagation(); }
                        else if view.move_selection(&event.keystroke.key, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(uniform_list("library-files", count, cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
                        let indices = view.visible(cx);
                        let compact = window.viewport_size().width < px(1100.);
                        range.map(|position| {
                            let record = &view.records[indices[position]];
                            let identity = (record.key, record.local_path.clone());
                            let selected = view.selected.as_ref() == Some(&identity);
                            let completed = record.status == DownloadStatus::Completed;
                            div().id(("library-row", position)).role(Role::ListBoxOption)
                                .aria_label(format!("{}. {}. {}", record.file_name, files::source(record), if completed { "Скачано" } else { "Не завершено" }))
                                .aria_selected(selected).aria_position_in_set(position + 1).aria_size_of_set(indices.len())
                                .when(selected, |row| row.aria_active_descendant().bg(theme.selection))
                                .h(px(56.)).w_full().px_3().flex().items_center().gap_4().border_b_1().border_color(theme.border)
                                .cursor_pointer().hover(move |style| style.bg(theme.selected))
                                .child(Icon::File.element())
                                .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
                                    .child(div().w_full().truncate().text_ellipsis_middle().child(record.file_name.clone()))
                                    .child(div().text_size(px(12.)).text_color(theme.muted).truncate().child(files::source(record))))
                                .child(format_badge(BookFormat::from_file_name(&record.file_name)))
                                .child(div().w(px(76.)).flex_shrink_0().text_right().text_sm().text_color(theme.muted)
                                    .child(file_size(record.progress.total_bytes().or(Some(record.progress.downloaded_bytes())))))
                                .when(!compact, |row| row.child(div().w(px(142.)).flex_shrink_0().text_right().text_size(px(12.)).text_color(theme.muted).child(files::date(record.added_at))))
                                .when(view.history, |row| row.child(div().w(px(100.)).flex_shrink_0().text_sm().text_color(if completed { theme.success } else { theme.warning }).child(if completed { "Скачано" } else { "Не завершено" })))
                                .on_click(cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                                    view.focus.focus(window, cx);
                                    view.selected = Some(identity.clone());
                                    if event.click_count() == 2 { view.activate(cx); }
                                    cx.notify();
                                }))
                        }).collect::<Vec<_>>()
                    })).track_scroll(&self.scroll).w_full().h_full())
            );
        // Column labels mirror row widths; narrow windows omit only the date.
        // The fixed action bar is outside the scrolling list.
        let selected = self.selected_record();
        root.child(
            div()
                .h(px(56.))
                .flex_shrink_0()
                .border_t_1()
                .border_color(theme.border)
                .pt_3()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(theme.muted)
                        .child(selected.map_or_else(
                            || format!("Показано {count} из {} файлов", self.records.len()),
                            |record| files::folder(&record.local_path),
                        )),
                )
                .when_some(
                    selected.filter(|record| record.status == DownloadStatus::Completed),
                    |bar, record| {
                        let path = record.local_path.clone();
                        let key_path = path.clone();
                        let copy_path = path.clone();
                        let key_copy_path = path.clone();
                        bar.child(
                            button("library-reveal", "Показать в папке")
                                .on_click(move |_, _, cx| platform::reveal_file(&path, cx))
                                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        platform::reveal_file(&key_path, cx);
                                        cx.stop_propagation();
                                    }
                                }),
                        )
                        .child(
                            primary_button("library-open", "Открыть")
                                .on_click(cx.listener(|view, _, _, cx| view.activate(cx)))
                                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        view.activate(cx);
                                        cx.stop_propagation();
                                    }
                                })),
                        )
                        .child(
                            icon_button("library-copy-path", "Копировать путь", Icon::Copy)
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        copy_path.clone(),
                                    ))
                                })
                                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            key_copy_path.clone(),
                                        ));
                                        cx.stop_propagation();
                                    }
                                }),
                        )
                    },
                ),
        )
        .when(count == 0, |root| {
            root.child(
                div()
                    .text_sm()
                    .text_color(theme.muted)
                    .child("Ничего не найдено. Измените поиск или формат."),
            )
        })
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use super::*;
    use gpui::{EntityInputHandler, TestAppContext};
    use tgbooks_core::DownloadProgress;

    fn record(index: i32, name: &str, status: DownloadStatus) -> DownloadRecord {
        let Ok(progress) = DownloadProgress::new(10, Some(10)) else {
            panic!("invalid fixture");
        };
        DownloadRecord {
            key: DownloadKey {
                channel_id: -1000000000001,
                message_id: index,
                document_id: i64::from(index),
            },
            file_name: name.into(),
            local_path: format!("/tmp/{name}"),
            source_title: Some("Книжная полка".into()),
            added_at: i64::from(index),
            status,
            progress,
        }
    }

    #[gpui::test]
    fn two_copies_of_one_book_have_independent_selection(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| FileTable::new(false, cx));
        let first = record(1, "book.epub", DownloadStatus::Completed);
        let mut second = first.clone();
        second.local_path = "/other/book.epub".into();
        second.added_at += 1;
        view.update(&mut cx.cx, |view, cx| {
            view.set_records(&[first.clone(), second.clone()], false, cx);
            assert_eq!(view.records.len(), 2);
            assert!(view.move_selection("home", cx));
            assert_eq!(view.selected_record(), Some(&second));
            assert!(view.move_selection("down", cx));
            assert_eq!(view.selected_record(), Some(&first));
        });
    }

    #[gpui::test]
    fn library_and_history_filter_real_records_and_clear_hidden_selection(cx: &mut TestAppContext) {
        let (library, cx) = cx.add_window_view(|_, cx| FileTable::new(false, cx));
        let records = vec![
            record(1, "Толстой.epub", DownloadStatus::Completed),
            record(2, "Учебник.pdf", DownloadStatus::Completed),
            record(3, "Архив.zip", DownloadStatus::Failed),
        ];
        library.update(&mut cx.cx, |view, cx| {
            view.set_records(&records, false, cx);
            assert_eq!(view.visible(cx), vec![1, 0]);
            view.selected = Some((records[1].key, records[1].local_path.clone()));
            view.format.update(cx, |select, cx| {
                select.selected = 2;
                cx.notify();
            });
        });
        cx.run_until_parked();
        library.read_with(&cx.cx, |view, cx| {
            assert_eq!(view.visible(cx), vec![0]);
            assert!(view.selected.is_none());
        });
        cx.update(|window, cx| {
            library.update(cx, |view, cx| {
                view.query.update(cx, |input, cx| {
                    input.replace_text_in_range(None, "КНИЖНАЯ", window, cx)
                });
            })
        });
        cx.run_until_parked();
        library.read_with(&cx.cx, |view, cx| assert_eq!(view.visible(cx), vec![0]));
        let history = cx.new(|cx| FileTable::new(true, cx));
        history.update(&mut cx.cx, |view, cx| {
            view.set_records(&records, false, cx);
            assert_eq!(view.records.len(), 3);
            assert_eq!(view.visible(cx), vec![2, 1, 0]);
        });
    }

    #[gpui::test]
    fn first_arrow_selects_first_file_and_end_reaches_virtualized_rows(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = FileTable::new(false, cx);
            let records: Vec<_> = (1..=1000)
                .map(|index| {
                    record(
                        index,
                        &format!("Книга {index}.epub"),
                        DownloadStatus::Completed,
                    )
                })
                .collect();
            view.set_records(&records, false, cx);
            view.focus.focus(window, cx);
            view
        });
        cx.simulate_keystrokes("down");
        view.read_with(&cx.cx, |view, _| {
            assert_eq!(
                view.selected.as_ref().map(|(key, _)| key.message_id),
                Some(1000)
            )
        });
        cx.simulate_keystrokes("end");
        view.read_with(&cx.cx, |view, _| {
            assert_eq!(
                view.selected.as_ref().map(|(key, _)| key.message_id),
                Some(1)
            );
            assert!(view.scroll.0.borrow().base_handle.offset().y < px(0.));
        });
    }

    #[gpui::test]
    fn keyboard_selection_and_copy_use_the_selected_file_not_its_display_name(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = FileTable::new(false, cx);
            view.set_records(
                &[record(1, "Книга.epub", DownloadStatus::Completed)],
                false,
                cx,
            );
            view.focus.focus(window, cx);
            view
        });
        cx.simulate_keystrokes("home");
        cx.update(|window, cx| {
            // Tab order after the list is reveal, open, copy.
            window.focus_next(cx);
            window.focus_next(cx);
            window.focus_next(cx);
        });
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("/tmp/Книга.epub".into())
            )
        });
        view.read_with(&cx.cx, |view, _| assert!(view.selected.is_some()));
    }
}
