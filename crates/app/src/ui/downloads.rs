use gpui::{
    AnyElement, Context, Div, FocusHandle, KeyDownEvent, Role, ScrollAnchor, ScrollHandle,
    Stateful, Subscription, Window, div, prelude::*, px, relative,
};
use tgbooks_core::{DownloadError, DownloadKey, DownloadRecord, DownloadStatus};

use super::{
    BookApp, Screen,
    channels::file_size,
    components::{button, empty_state, format_badge, icon_button, primary_button},
    files,
    icons::Icon,
    is_activation_key, metrics,
    theme::Theme,
};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum Control {
    Active,
    Queue,
    Waiting(DownloadKey),
}

pub(super) struct ScrollControl {
    pub focus: FocusHandle,
    anchor: ScrollAnchor,
    observer: Option<Subscription>,
}

impl ScrollControl {
    pub fn new(focus: FocusHandle, scroll: ScrollHandle) -> Self {
        Self {
            focus,
            anchor: ScrollAnchor::for_handle(scroll),
            observer: None,
        }
    }

    pub fn observe(&mut self, window: &mut Window, cx: &mut Context<BookApp>) {
        if self.observer.is_some() {
            return;
        }
        let target = self.anchor.clone();
        self.observer = Some(cx.on_focus(&self.focus, window, move |_, window, cx| {
            target.scroll_to(window, cx);
            let entity = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(entity));
        }));
    }
}

fn control(app: &BookApp, id: Control, button: Stateful<Div>) -> Stateful<Div> {
    button.when_some(app.download_controls.get(&id), |button, scroll| {
        button
            .track_focus(&scroll.focus)
            .anchor_scroll(Some(scroll.anchor.clone()))
    })
}

pub(super) fn queue_control(app: &BookApp, cx: &Context<BookApp>) -> AnyElement {
    if app.transfer.queued.is_empty() {
        return div().into_any_element();
    }
    let label = if app.transfer.paused {
        "Продолжить очередь"
    } else {
        "Приостановить очередь"
    };
    control(
        app,
        Control::Queue,
        if app.transfer.paused {
            primary_button("toggle-queue", label)
        } else {
            button("toggle-queue", label)
        },
    )
    .child(
        if app.transfer.paused {
            Icon::Play
        } else {
            Icon::Pause
        }
        .element(),
    )
    .text_sm()
    .flex_shrink_0()
    .on_click(cx.listener(|app, _, _, cx| app.toggle_queue(cx)))
    .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
        if is_activation_key(event) {
            app.toggle_queue(cx);
            cx.stop_propagation();
        }
    }))
    .into_any_element()
}

pub(super) fn tabs(app: &BookApp, cx: &Context<BookApp>) -> Div {
    let theme = Theme::dark();
    div()
        .flex()
        .gap_3()
        .flex_shrink_0()
        .border_b_1()
        .border_color(theme.border)
        .children([false, true].into_iter().map(|history| {
            button(
                if history {
                    "downloads-history"
                } else {
                    "downloads-queue"
                },
                if history {
                    "История".into()
                } else {
                    format!(
                        "Очередь {}",
                        app.transfer.queued.len() + usize::from(app.transfer.active.is_some())
                    )
                },
            )
            .aria_selected(app.show_history == history)
            .border_color(theme.canvas)
            .rounded_none()
            .border_b_2()
            .when(app.show_history == history, |tab| {
                tab.border_color(theme.accent)
            })
            .on_click(cx.listener(move |app, _, window, cx| {
                app.show_history = history;
                app.focus_download = false;
                app.focus_handle.focus(window, cx);
                cx.notify();
            }))
            .on_key_down(cx.listener(move |app, event: &KeyDownEvent, window, cx| {
                if is_activation_key(event) {
                    app.show_history = history;
                    app.focus_download = false;
                    app.focus_handle.focus(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
        }))
}

pub(super) fn render(app: &BookApp, cx: &Context<BookApp>) -> Div {
    let theme = Theme::dark();
    let mut list = div().flex().flex_col().gap_4();
    if let Some(error) = app.transfer.error {
        let text = error_message(error);
        list = list.child(
            div()
                .id("download-error")
                .role(Role::Alert)
                .aria_label(text.clone())
                .text_sm()
                .text_color(theme.error)
                .when_some(app.transfer.error_file.clone(), |message, file| {
                    message.child(div().truncate().text_ellipsis_middle().child(file))
                })
                .child(text),
        );
    }
    if app.transfer.restored {
        list = list.child(div().id("queue-restored").p_3().rounded_md().bg(theme.selection)
            .text_sm().text_color(theme.accent)
            .child("Очередь восстановлена на паузе. Продолжите вручную; сохранённая часть файла будет проверена перед докачкой."));
    }
    if let Some(key) = app.transfer.active {
        let cancel = (!app.transfer.cancelling).then(|| {
            control(
                app,
                Control::Active,
                icon_button("cancel-download", "Отменить загрузку", Icon::Close),
            )
            .on_click(cx.listener(|app, _, _, cx| app.cancel_download(cx)))
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                if is_activation_key(event) {
                    app.cancel_download(cx);
                    cx.stop_propagation();
                }
            }))
        });
        let record = app.records.iter().find(|record| {
            record.key == key
                && matches!(
                    record.status,
                    DownloadStatus::Queued | DownloadStatus::Downloading
                )
        });
        list =
            list.child(
                div()
                    .p_4()
                    .bg(theme.sidebar)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_sm().text_color(theme.accent).child(
                                if app.transfer.cancelling {
                                    "Отменяем загрузку…"
                                } else if record.is_some_and(|record| {
                                    record.status == DownloadStatus::Downloading
                                        && record.progress.total_bytes()
                                            == Some(record.progress.downloaded_bytes())
                                }) {
                                    "Сохраняем файл…"
                                } else if record.is_some_and(|record| {
                                    record.status == DownloadStatus::Downloading
                                }) {
                                    "Скачивается"
                                } else {
                                    "Проверяем вложение в Telegram…"
                                },
                            ))
                            .children(cancel),
                    )
                    .when_some(record, |panel, record| {
                        panel.child(active_record(app, record))
                    }),
            );
    }
    if !app.transfer.queued.is_empty() {
        list = list
            .child(
                div()
                    .flex()
                    .justify_between()
                    .pt_3()
                    .text_sm()
                    .text_color(theme.muted)
                    .child(if app.transfer.paused {
                        "Очередь приостановлена"
                    } else {
                        "Следующие"
                    })
                    .child(files::count(
                        app.transfer.queued.len(),
                        "файл",
                        "файла",
                        "файлов",
                    )),
            )
            .children(app.transfer.queued.iter().enumerate().map(|(index, job)| {
                let key = job.book.key;
                div()
                    .id(("queued-download", index))
                    .h(px(64.))
                    .flex()
                    .items_center()
                    .gap_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .w(px(18.))
                            .text_sm()
                            .text_color(theme.muted)
                            .child((index + 1).to_string()),
                    )
                    .child(Icon::File.element())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .truncate()
                                    .text_ellipsis_middle()
                                    .child(job.book.file_name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(if job.interrupted {
                                        theme.warning
                                    } else {
                                        theme.muted
                                    })
                                    .child(if job.interrupted {
                                        "Прервано · Доступна проверка и докачка".into()
                                    } else {
                                        files::folder_path(&job.directory)
                                    }),
                            ),
                    )
                    .child(format_badge(Some(job.book.format)))
                    .child(
                        div()
                            .w(px(80.))
                            .text_right()
                            .text_sm()
                            .text_color(theme.muted)
                            .child(file_size(job.book.size_bytes)),
                    )
                    .child(
                        control(
                            app,
                            Control::Waiting(key),
                            icon_button(("cancel-queued", index), "Убрать из очереди", Icon::Close),
                        )
                        .debug_selector(move || format!("cancel-queued-{index}"))
                        .aria_label(format!("Убрать из очереди: {}", job.book.file_name))
                        .on_click(
                            cx.listener(move |app, _, window, cx| {
                                app.cancel_queued(key, window, cx)
                            }),
                        )
                        .on_key_down(cx.listener(
                            move |app, event: &KeyDownEvent, window, cx| {
                                if is_activation_key(event) {
                                    app.cancel_queued(key, window, cx);
                                    cx.stop_propagation();
                                }
                            },
                        )),
                    )
            }));
    }
    if app.transfer.active.is_none() && app.transfer.queued.is_empty() {
        list = list.child(empty_state("В очереди пока нет файлов", "Выберите книги в разделе «Каналы». Результаты завершённых и прерванных загрузок доступны во вкладке «История»."))
            .child(primary_button("queue-go-channels", "Перейти к каналам")
                .on_click(cx.listener(|app, _, _, cx| app.navigate(Screen::Channels, cx)))
                .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                    if is_activation_key(event) { app.navigate(Screen::Channels, cx); cx.stop_propagation(); }
                })));
    } else {
        list = list.child(div().pt_4().text_size(px(12.)).text_color(theme.muted)
            .child("Очередь сохранена. После перезапуска — ручное продолжение. Пауза не прерывает текущий файл."));
    }
    list.when(app.transfer.skipped_duplicates > 0, |list| {
        list.child(div().text_sm().text_color(theme.muted).child(format!(
            "Пропущено повторов: {}",
            app.transfer.skipped_duplicates
        )))
    })
}

fn active_record(app: &BookApp, record: &DownloadRecord) -> Div {
    let theme = Theme::dark();
    let values = app
        .metrics
        .filter(|(key, _)| *key == record.key && !app.transfer.cancelling)
        .map(|(_, display)| display.values)
        .unwrap_or_default();
    let (speed, time) = metrics::labels(record.progress, values);
    let progress = record.progress.total_bytes().map_or_else(
        || {
            format!(
                "{} · размер неизвестен",
                file_size(Some(record.progress.downloaded_bytes()))
            )
        },
        |total| {
            format!(
                "{} из {}",
                file_size(Some(record.progress.downloaded_bytes())),
                file_size(Some(total))
            )
        },
    );
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .text_ellipsis_middle()
                                .child(record.file_name.clone()),
                        )
                        .child(div().text_sm().text_color(theme.muted).child(progress))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .text_size(px(12.))
                                .text_color(theme.muted)
                                .child(Icon::Channels.element())
                                .child(div().truncate().child(files::folder(&record.local_path))),
                        ),
                )
                .child(
                    div()
                        .id("download-metrics")
                        .role(Role::Group)
                        .aria_label(format!(
                            "Скорость загрузки: {speed}. Примерное оставшееся время: {time}"
                        ))
                        .w(px(112.))
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .text_right()
                        .font_features(gpui::FontFeatures(std::sync::Arc::new(vec![(
                            "tnum".into(),
                            1,
                        )])))
                        .child(
                            div()
                                .debug_selector(|| "download-speed".into())
                                .w_full()
                                .truncate()
                                .child(speed),
                        )
                        .child(
                            div()
                                .debug_selector(|| "download-eta".into())
                                .w_full()
                                .truncate()
                                .text_color(theme.muted)
                                .child(time),
                        ),
                ),
        )
        .when_some(record.progress.percentage(), |row, percent| {
            row.child(
                div()
                    .id("active-progress")
                    .role(Role::ProgressIndicator)
                    .aria_label(format!("Загрузка {}", record.file_name))
                    .aria_min_numeric_value(0.)
                    .aria_max_numeric_value(100.)
                    .aria_numeric_value(f64::from(percent))
                    .h(px(4.))
                    .w_full()
                    .bg(theme.border)
                    .rounded_sm()
                    .child(
                        div()
                            .h_full()
                            .w(relative(f32::from(percent) / 100.))
                            .bg(theme.accent),
                    ),
            )
        })
}

pub(super) fn error_message(error: DownloadError) -> String {
    match error {
        DownloadError::Busy => "Уже выполняется загрузка. Дождитесь окончания или отмените её.",
        DownloadError::QueueFull => "В ожидании допускается до 256 книг. Выберите меньше файлов.",
        DownloadError::Unauthorized => "Сессия Telegram недействительна. Подключите аккаунт заново.",
        DownloadError::Unavailable => "Вложение удалено, изменено или больше не доступно.",
        DownloadError::ProtectedContent => "Владелец запретил сохранение этого контента.",
        DownloadError::FloodWait(seconds) => return format!("Telegram ограничил запросы на {seconds} с. Задание сохранено. После ожидания продолжите очередь вручную."),
        DownloadError::Network => "Сетевой сбой или таймаут. Задание и скачанная часть сохранены. Проверьте соединение и продолжите очередь.",
        DownloadError::SecureStorage => "Не удалось сохранить защищённую Telegram-сессию. Проверьте системное хранилище ключей.",
        DownloadError::Storage => "Ошибка SQLite. Проверьте папку: файл уже мог сохраниться. Существующие файлы не перезаписываются.",
        DownloadError::Destination => "Папка недоступна для записи. Очередь приостановлена. Восстановите доступ и продолжите либо уберите задания и добавьте их в другую папку.",
        DownloadError::FileExists => "Файл с таким именем уже существует. Он не перезаписан; выберите другую папку.",
        DownloadError::AlreadyDownloaded => "Эта книга уже есть в истории. Найдите её в библиотеке.",
        DownloadError::InvalidFile => "Некорректное имя, формат или идентификатор. Повторите поиск в канале.",
        DownloadError::Incomplete => "Размер файла не совпал с ожидаемым. Неполный файл не сохранён.",
        DownloadError::Cancelled => "Загрузка отменена. Повторить можно из раздела «Каналы».",
        DownloadError::UnsupportedReply => "Grammers не поддерживает ответ Telegram. Загрузка остановлена.",
        DownloadError::Internal => "Непредвиденная ошибка загрузки. Проверьте папку и повторите действие.",
    }.into()
}
