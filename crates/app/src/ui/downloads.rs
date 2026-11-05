use gpui::{
    Context, Div, FocusHandle, KeyDownEvent, Role, ScrollAnchor, ScrollHandle, Stateful,
    Subscription, Window, div, prelude::*, px, relative,
};
use tgbooks_core::{DownloadError, DownloadKey, DownloadRecord, DownloadStatus};

use super::{
    BookApp, Screen,
    channels::file_size,
    components::{button, empty_state},
    is_activation_key, metrics,
    theme::Theme,
};
use crate::platform;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum Control {
    Active,
    Queue,
    Waiting(DownloadKey),
    Reveal(DownloadKey),
}

pub(super) struct ScrollControl {
    pub focus: FocusHandle,
    anchor: ScrollAnchor,
    observer: Option<Subscription>,
}

impl ScrollControl {
    pub fn new(focus: FocusHandle, scroll: ScrollHandle) -> Self {
        let anchor = ScrollAnchor::for_handle(scroll);
        Self {
            focus,
            anchor,
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
            // ScrollAnchor changes the offset after the frame, so redraw then.
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

pub(super) fn render(app: &BookApp, cx: &Context<BookApp>) -> Div {
    let theme = Theme::dark();
    let mut list = div().flex().flex_col().gap_3();
    if app.screen != Screen::Library {
        if let Some(error) = app.transfer.error {
            let text = error_message(error);
            list = list.child(
                div()
                    .id("download-error")
                    .role(Role::Alert)
                    .aria_label(text.clone())
                    .text_color(theme.error)
                    .when_some(app.transfer.error_file.clone(), |message, file| {
                        message.child(div().truncate().text_ellipsis_middle().child(file))
                    })
                    .child(text),
            );
        }
        if let Some(key) = app.transfer.active {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child(if app.transfer.cancelling {
                                "Отменяем текущую загрузку…"
                            } else if app.records.iter().any(|record| {
                                record.key == key
                                    && matches!(
                                        record.status,
                                        DownloadStatus::Queued | DownloadStatus::Downloading
                                    )
                            }) {
                                "Загрузка выполняется"
                            } else {
                                "Проверяем доступ и вложение в Telegram…"
                            }),
                    )
                    .when(!app.transfer.cancelling, |panel| {
                        panel.child(
                            control(
                                app,
                                Control::Active,
                                button("cancel-download", "Отменить загрузку"),
                            )
                            .on_click(cx.listener(|app, _, _, cx| app.cancel_download(cx)))
                            .on_key_down(cx.listener(
                                |app, event: &KeyDownEvent, _, cx| {
                                    if is_activation_key(event) {
                                        app.cancel_download(cx);
                                        cx.stop_propagation();
                                    }
                                },
                            )),
                        )
                    }),
            );
        }
        if let Some(record) = app
            .records
            .iter()
            .find(|record| Some(record.key) == app.transfer.active)
        {
            list = list.child(record_row(app, 0, record));
        }
        if !app.transfer.queued.is_empty() {
            list = list
                .child(
                    div().flex().flex_col().gap_2()
                        .child(div().text_color(theme.muted).child(format!(
                            "{} · Ожидают: {} · Одновременно: 1",
                            if app.transfer.paused { "Очередь приостановлена" } else { "Последовательная очередь" },
                            app.transfer.queued.len(),
                        )))
                        .child(control(app, Control::Queue, button("toggle-queue", if app.transfer.paused { "Продолжить очередь" } else { "Приостановить очередь" }))
                            .on_click(cx.listener(|app, _, _, cx| app.toggle_queue(cx)))
                            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                                if is_activation_key(event) {
                                    app.toggle_queue(cx);
                                    cx.stop_propagation();
                                }
                            })))
                        .child(div().text_sm().text_color(theme.muted).child("Пауза не прерывает текущий файл. Ожидающие задания пока не восстанавливаются после закрытия приложения.")),
                )
                .children(app.transfer.queued.iter().enumerate().map(|(index, job)| {
                    let key = job.book.key;
                    div().id(("queued-download", index)).flex().flex_col().gap_2()
                        .py_3().border_b_1().border_color(theme.border)
                        .child(div().truncate().text_ellipsis_middle().child(format!("{}. {}", index + 1, job.book.file_name)))
                        .child(div().text_sm().text_color(theme.muted).child("Ожидает загрузки"))
                        .child(div().text_sm().text_color(theme.muted).truncate().child(job.directory.to_string_lossy().into_owned()))
                        .child(control(app, Control::Waiting(key), button(("cancel-queued", index), "Убрать из очереди"))
                            .debug_selector(move || format!("cancel-queued-{index}"))
                            .aria_label(format!("Убрать из очереди: {}", job.book.file_name))
                            .on_click(cx.listener(move |app, _, window, cx| app.cancel_queued(key, window, cx)))
                            .on_key_down(cx.listener(move |app, event: &KeyDownEvent, window, cx| {
                                if is_activation_key(event) {
                                    app.cancel_queued(key, window, cx);
                                    cx.stop_propagation();
                                }
                            })))
                }));
        }
        if app.transfer.skipped_duplicates > 0 {
            list = list.child(div().text_sm().text_color(theme.muted).child(format!(
                "Пропущено повторов при последнем добавлении: {}",
                app.transfer.skipped_duplicates
            )));
        }
    }
    let records: Vec<_> = app
        .records
        .iter()
        .filter(|record| {
            if app.screen == Screen::Library {
                record.status == DownloadStatus::Completed
            } else {
                Some(record.key) != app.transfer.active
            }
        })
        .collect();
    if records.is_empty() {
        if (app.transfer.active.is_none() && app.transfer.queued.is_empty())
            || app.screen == Screen::Library
        {
            list = list.child(empty_state(
                if app.loading { "Читаем локальную историю…" }
                else if app.screen == Screen::Library { "Библиотека пока пуста" }
                else { "Загрузок пока нет" },
                "Отметьте книги в разделе «Каналы» и нажмите «Добавить выбранные в очередь…». История начатых загрузок сохраняется на этом компьютере.",
            ));
        }
        return list;
    }
    list.children(
        records
            .into_iter()
            .enumerate()
            .map(|(index, record)| record_row(app, index + 1, record)),
    )
}

fn record_row(app: &BookApp, index: usize, record: &DownloadRecord) -> Stateful<Div> {
    let theme = Theme::dark();
    let finalizing = record.status == DownloadStatus::Downloading
        && record.progress.total_bytes() == Some(record.progress.downloaded_bytes());
    let status = match record.status {
        DownloadStatus::Queued => "Подготовка",
        DownloadStatus::Downloading if finalizing => "Сохраняем файл…",
        DownloadStatus::Downloading => "Скачивается",
        DownloadStatus::Completed => "Скачано",
        DownloadStatus::Failed => "Ошибка или прервано",
    };
    let bytes = file_size(Some(record.progress.downloaded_bytes()));
    let progress = record.progress.percentage().map_or_else(
        || format!("{status} · {bytes} · размер неизвестен"),
        |percent| {
            format!(
                "{status} · {percent}% · {bytes} из {}",
                file_size(record.progress.total_bytes())
            )
        },
    );
    let active =
        record.status == DownloadStatus::Downloading && app.transfer.active == Some(record.key);
    let details = div()
        .flex()
        .items_start()
        .gap_4()
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    div()
                        .truncate()
                        .text_ellipsis_middle()
                        .child(record.file_name.clone()),
                )
                .child(div().text_sm().text_color(theme.muted).child(progress)),
        )
        .when(active, |details| {
            let values = app
                .metrics
                .filter(|(key, _)| *key == record.key && !app.transfer.cancelling)
                .map(|(_, display)| display.values)
                .unwrap_or_default();
            let (speed, time) = metrics::labels(record.progress, values);
            details.child(
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
                    .text_color(theme.muted)
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
                            .child(time),
                    ),
            )
        });
    let mut row = div()
        .id(("download-record", index))
        .flex()
        .flex_col()
        .gap_2()
        .py_4()
        .border_b_1()
        .border_color(theme.border)
        .child(details);
    if let Some(percent) = record.progress.percentage() {
        row = row.child(
            div()
                .id(("download-progress", index))
                .role(Role::ProgressIndicator)
                .aria_label(format!("Загрузка {}", record.file_name))
                .aria_min_numeric_value(0.)
                .aria_max_numeric_value(100.)
                .aria_numeric_value(f64::from(percent))
                .h(px(4.))
                .w_full()
                .bg(theme.border)
                .rounded_sm()
                .child(div().h_full().w(relative(f32::from(percent) / 100.)).bg(
                    if record.status == DownloadStatus::Failed {
                        theme.error
                    } else {
                        theme.accent
                    },
                )),
        );
    }
    row = row.child(
        div()
            .text_sm()
            .text_color(theme.muted)
            .truncate()
            .child(record.local_path.clone()),
    );
    if record.status == DownloadStatus::Completed {
        let path = record.local_path.clone();
        let key_path = path.clone();
        row = row.child(
            control(
                app,
                Control::Reveal(record.key),
                button(("reveal-download", index), "Показать в папке"),
            )
            .on_click(move |_, _, cx| platform::reveal_file(&path, cx))
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if is_activation_key(event) {
                    platform::reveal_file(&key_path, cx);
                    cx.stop_propagation();
                }
            }),
        );
    }
    row
}

pub(super) fn error_message(error: DownloadError) -> String {
    match error {
        DownloadError::Busy => "Уже выполняется загрузка. Дождитесь окончания или отмените её.",
        DownloadError::QueueFull => "За одно добавление и в ожидании допускается до 256 книг. Выберите меньше книг или дождитесь освобождения очереди.",
        DownloadError::Unauthorized => "Сессия Telegram недействительна. Подключите аккаунт заново в разделе «Вход».",
        DownloadError::Unavailable => "Вложение удалено, изменено или больше не доступно. Выберите другую книгу или канал.",
        DownloadError::ProtectedContent => "Владелец запретил сохранение этого контента. Загрузка не выполняется.",
        DownloadError::FloodWait(seconds) => return format!("Telegram ограничил запросы на {seconds} с. Очередь приостановлена. После ожидания нажмите «Продолжить очередь». Неудавшуюся книгу добавьте заново; автоматических повторов нет."),
        DownloadError::Network => "Загрузка прервана: Telegram не ответил за 30 секунд или произошёл сетевой сбой. Повторите из раздела «Каналы».",
        DownloadError::SecureStorage => "Не удалось сохранить защищённую Telegram-сессию. Проверьте доступ к системному хранилищу ключей и данным.",
        DownloadError::Storage => "Ошибка SQLite. Файл уже мог сохраниться: проверьте выбранную папку. Существующие файлы не перезаписываются.",
        DownloadError::Destination => "Не удалось записать файл. Выберите доступную папку и проверьте свободное место.",
        DownloadError::FileExists => "Файл с таким именем уже существует и не был перезаписан. Выберите другую папку или переместите существующий файл.",
        DownloadError::AlreadyDownloaded => "Эта книга уже есть в истории. Откройте «Библиотеку», чтобы найти сохранённый файл.",
        DownloadError::InvalidFile => "Некорректное имя, формат или идентификатор вложения. Повторите поиск в канале.",
        DownloadError::Incomplete => "Размер полученного файла не совпал с ожидаемым. Неполный файл не опубликован; повторите загрузку.",
        DownloadError::Cancelled => "Загрузка отменена. Повтор можно начать из раздела «Каналы».",
        DownloadError::UnsupportedReply => "Grammers не поддерживает ответ Telegram. Загрузка остановлена без обхода ограничений.",
        DownloadError::Internal => "Загрузка завершилась непредвиденно. Проверьте папку и повторите действие.",
    }.into()
}
