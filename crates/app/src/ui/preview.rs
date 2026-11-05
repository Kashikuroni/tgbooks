//! Opt-in development previews, rendered with the same GPUI tree and native
//! fonts as the application. No desktop capture or Telegram account is needed.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use gpui::{HeadlessAppContext, prelude::*, px, size};
use tgbooks_core::{
    AppInfo, AuthState, AuthStep, BookAttachment, BookFormat, CatalogError, CatalogOperation,
    CatalogState, ChannelSummary, CoreEvent, DownloadError, DownloadKey, DownloadProgress,
    DownloadRecord, DownloadStatus, QueuedDownload, TransferMetrics, TransferState,
};

use super::{BookApp, Screen, icons::IconAssets};

pub fn render() -> Result<()> {
    // MacPlatform must be initialized on the main thread, not a Rust test worker.
    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(IconAssets),
        gpui_platform::current_headless_renderer,
    );
    let output = std::env::var_os("TGBOOKS_PREVIEW_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-previews")
        });
    std::fs::create_dir_all(&output)?;
    let (ui, _backend) = tgbooks_core::connect();
    let window = cx.open_window(size(px(1040.), px(700.)), |_, cx| {
        cx.new(|cx| BookApp::new(ui, cx))
    })?;
    let app = window.root(&mut cx)?;
    let info = AppInfo {
        data_directory: "~/Library/Application Support/org.tgbooks.TelegramBookDownloader".into(),
        download_directory:
            "~/Library/Application Support/org.tgbooks.TelegramBookDownloader/books".into(),
    };
    app.update(&mut cx, |app, cx| {
        app.apply_event(CoreEvent::Ready(info.clone()), cx);
        app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
        app.apply_event(CoreEvent::Authentication(AuthState::default()), cx);
    });
    for (index, screen) in Screen::ALL.into_iter().enumerate() {
        app.update(&mut cx, |app, cx| app.navigate(screen, cx));
        save_frame(
            &mut cx,
            window.into(),
            &output,
            &format!("{index}-screen.png"),
        )?;
    }
    app.update(&mut cx, |app, cx| {
        app.sidebar_open = false;
        app.navigate(Screen::Library, cx);
    });
    save_frame(&mut cx, window.into(), &output, "sidebar-hidden.png")?;

    let (ui, _compact_backend) = tgbooks_core::connect();
    let compact = cx.open_window(size(px(760.), px(520.)), |_, cx| {
        cx.new(|cx| BookApp::new(ui, cx))
    })?;
    let compact_app = compact.root(&mut cx)?;
    compact_app.update(&mut cx, |app, cx| {
        app.apply_event(CoreEvent::Ready(info), cx);
        app.apply_event(CoreEvent::HistoryLoaded(Vec::new()), cx);
        app.navigate(Screen::Settings, cx);
    });
    save_frame(&mut cx, compact.into(), &output, "compact-settings.png")?;
    compact_app.update(&mut cx, |app, cx| {
        app.apply_event(CoreEvent::Authentication(AuthState::default()), cx);
        app.navigate(Screen::Login, cx);
    });
    save_frame(&mut cx, compact.into(), &output, "compact-login.png")?;
    for (name, step) in [
        ("phone", AuthStep::Phone),
        ("code", AuthStep::Code),
        ("password", AuthStep::Password),
        ("authorized", AuthStep::Authorized),
    ] {
        app.update(&mut cx, |app, cx| {
            app.apply_event(
                CoreEvent::Authentication(AuthState {
                    step,
                    ..Default::default()
                }),
                cx,
            );
            app.navigate(Screen::Login, cx);
        });
        save_frame(&mut cx, window.into(), &output, &format!("auth-{name}.png"))?;
    }
    for (name, state) in [
        ("channels-list", catalog_fixture(false)),
        ("channels-books", catalog_fixture(true)),
        ("channels-protected", {
            let mut state = catalog_fixture(false);
            state.selected = Some(state.channels[2].id);
            state
        }),
        (
            "channels-loading",
            CatalogState {
                authenticated: true,
                operation: Some(CatalogOperation::ListChannels),
                ..Default::default()
            },
        ),
        ("channels-error", {
            let mut state = catalog_fixture(true);
            state.error = Some(CatalogError::FloodWait(60));
            state
        }),
    ] {
        app.update(&mut cx, |app, cx| {
            app.sidebar_open = true;
            app.apply_event(CoreEvent::Catalog(state), cx);
            app.navigate(Screen::Channels, cx);
        });
        save_frame(&mut cx, window.into(), &output, &format!("{name}.png"))?;
    }
    for (name, state) in [
        ("compact-channels", catalog_fixture(false)),
        ("compact-books", catalog_fixture(true)),
        ("compact-books-error", {
            let mut state = catalog_fixture(true);
            state.error = Some(CatalogError::FloodWait(60));
            state
        }),
        ("compact-before-scan", {
            let mut state = catalog_fixture(false);
            state.selected = Some(state.channels[0].id);
            state
        }),
    ] {
        compact_app.update(&mut cx, |app, cx| {
            app.apply_event(
                CoreEvent::Authentication(AuthState {
                    step: AuthStep::Authorized,
                    ..Default::default()
                }),
                cx,
            );
            app.apply_event(CoreEvent::Catalog(state), cx);
            app.navigate(Screen::Channels, cx);
        });
        save_frame(&mut cx, compact.into(), &output, &format!("{name}.png"))?;
    }
    for (name, status, bytes, total, error, screen) in [
        (
            "download-progress",
            DownloadStatus::Downloading,
            38_000_000,
            Some(80_000_000),
            None,
            Screen::Downloads,
        ),
        (
            "download-unknown-size",
            DownloadStatus::Downloading,
            2_560_000,
            None,
            None,
            Screen::Downloads,
        ),
        (
            "download-completed",
            DownloadStatus::Completed,
            80_000_000,
            Some(80_000_000),
            None,
            Screen::Downloads,
        ),
        (
            "download-error",
            DownloadStatus::Failed,
            38_000_000,
            Some(80_000_000),
            Some(DownloadError::Network),
            Screen::Downloads,
        ),
        (
            "library-completed",
            DownloadStatus::Completed,
            80_000_000,
            Some(80_000_000),
            None,
            Screen::Library,
        ),
        (
            "queue-active",
            DownloadStatus::Downloading,
            38_000_000,
            Some(80_000_000),
            None,
            Screen::Downloads,
        ),
        (
            "queue-paused",
            DownloadStatus::Failed,
            38_000_000,
            Some(80_000_000),
            Some(DownloadError::FloodWait(30)),
            Screen::Downloads,
        ),
        (
            "download-large-zip",
            DownloadStatus::Downloading,
            268_435_456,
            Some(1_503_238_554),
            None,
            Screen::Downloads,
        ),
        (
            "download-stalled",
            DownloadStatus::Downloading,
            268_435_456,
            Some(1_503_238_554),
            None,
            Screen::Downloads,
        ),
        (
            "download-measuring",
            DownloadStatus::Downloading,
            0,
            Some(1_503_238_554),
            None,
            Screen::Downloads,
        ),
        (
            "download-finalizing",
            DownloadStatus::Downloading,
            1_503_238_554,
            Some(1_503_238_554),
            None,
            Screen::Downloads,
        ),
    ] {
        let mut record = download_fixture(status, bytes, total)?;
        if total == Some(1_503_238_554) {
            record.file_name = "Полное собрание книг — 1.4 GB.zip".into();
            record.local_path = format!("/Users/demo/Книги/{}", record.file_name);
        }
        let transfer = TransferState {
            active: (status == DownloadStatus::Downloading).then_some(record.key),
            error,
            queued: if name.starts_with("queue-") {
                (1..=2)
                    .map(|index| {
                        let mut key = record.key;
                        key.message_id += index;
                        key.document_id += i64::from(index);
                        QueuedDownload {
                            book: BookAttachment {
                                key,
                                file_name: format!("Книга {index} — A very long title 世界.epub"),
                                size_bytes: Some(5_000_000),
                                format: BookFormat::Epub,
                            },
                            directory: "/Users/reader/Books/Telegram library".into(),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            },
            paused: name == "queue-paused",
            error_file: error.map(|_| record.file_name.clone()),
            ..Default::default()
        };
        // Isolate transfer snapshots from the previous window's cached scene.
        // Transitions themselves are exercised by the interactive GPUI tests.
        for (prefix, bounds) in [
            ("", size(px(1040.), px(700.))),
            ("compact-", size(px(760.), px(520.))),
        ] {
            let (ui, _snapshot_backend) = tgbooks_core::connect();
            let snapshot = cx.open_window(bounds, |_, cx| cx.new(|cx| BookApp::new(ui, cx)))?;
            let view = snapshot.root(&mut cx)?;
            view.update(&mut cx, |app, cx| {
                app.apply_event(
                    CoreEvent::Authentication(AuthState {
                        step: AuthStep::Authorized,
                        ..Default::default()
                    }),
                    cx,
                );
                app.apply_event(CoreEvent::HistoryLoaded(vec![record.clone()]), cx);
                app.apply_event(CoreEvent::DownloadState(transfer.clone()), cx);
                if status == DownloadStatus::Downloading && name != "download-measuring" {
                    let rate = if name == "download-stalled" {
                        0
                    } else if name == "download-large-zip" {
                        524_288
                    } else {
                        2_097_152
                    };
                    let remaining_seconds = total
                        .filter(|_| rate > 0)
                        .and_then(|total| total.checked_sub(bytes))
                        .filter(|remaining| *remaining > 0)
                        .map(|remaining| remaining.div_ceil(rate));
                    app.apply_event(
                        CoreEvent::DownloadMetrics {
                            key: record.key,
                            metrics: TransferMetrics {
                                bytes_per_second: Some(rate),
                                remaining_seconds,
                            },
                        },
                        cx,
                    );
                }
                app.navigate(screen, cx);
            });
            save_frame(
                &mut cx,
                snapshot.into(),
                &output,
                &format!("{prefix}{name}.png"),
            )?;
            cx.update_window(snapshot.into(), |_, window, _| window.remove_window())?;
        }
    }
    println!("UI previews: {}", output.display());
    Ok(())
}

fn download_fixture(
    status: DownloadStatus,
    bytes: u64,
    total: Option<u64>,
) -> Result<DownloadRecord> {
    let file_name =
        "book-Архитектура_современных_операционных_систем_полное_издание--1000000000001-9100-1.pdf";
    Ok(DownloadRecord {
        key: DownloadKey {
            channel_id: -1000000000001,
            message_id: 9100,
            document_id: 1,
        },
        file_name: file_name.into(),
        local_path: format!("/Users/demo/Книги/{file_name}"),
        status,
        progress: DownloadProgress::new(bytes, total)?,
    })
}

fn save_frame(
    cx: &mut HeadlessAppContext,
    window: gpui::AnyWindowHandle,
    output: &Path,
    name: &str,
) -> Result<()> {
    // Drain focus/event updates, then capture a full draw synchronously. The
    // simulated frame clock can otherwise leave a partially reused scene.
    cx.run_until_parked();
    let image = cx.update_window(window, |_, window, cx| {
        window.refresh();
        let arena = window.draw(cx);
        let image = window.render_to_image();
        arena.clear(cx);
        image
    })??;
    image.save(output.join(name))?;
    Ok(())
}

fn catalog_fixture(selected: bool) -> CatalogState {
    let channels = vec![
        ChannelSummary {
            id: -1000000000001,
            title: "Книжная полка".into(),
            username: Some("book_shelf".into()),
            protected: false,
        },
        ChannelSummary {
            id: -1000000000002,
            title: "Научная библиотека".into(),
            username: None,
            protected: false,
        },
        ChannelSummary {
            id: -1000000000003,
            title: "Архив редких изданий".into(),
            username: None,
            protected: true,
        },
        ChannelSummary {
            id: -1000000000004,
            title: "Очень длинное название канала с книгами и журналами на разных языках 世界の本"
                .into(),
            username: Some("world_library".into()),
            protected: false,
        },
    ];
    let books = if selected {
        [
            (
                "Лев Толстой — Война и мир.epub",
                BookFormat::Epub,
                Some(2_560_000),
            ),
            (
                "Архитектура_современных_операционных_систем_полное_издание_2026.pdf",
                BookFormat::Pdf,
                Some(74_200_000),
            ),
            ("История мировой литературы.fb2", BookFormat::Fb2, None),
            (
                "世界の本 — Избранное.zip",
                BookFormat::Zip,
                Some(142_000_000),
            ),
            ("Атлас.djvu", BookFormat::Djvu, Some(8_920_000)),
            ("Сборник.mobi", BookFormat::Mobi, Some(1_024_000)),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, format, size))| BookAttachment {
            key: DownloadKey {
                channel_id: channels[0].id,
                message_id: 9100 - index as i32,
                document_id: index as i64 + 1,
            },
            file_name: name.into(),
            size_bytes: size,
            format,
        })
        .collect()
    } else {
        Vec::new()
    };
    CatalogState {
        authenticated: true,
        selected: selected.then_some(channels[0].id),
        channels,
        books,
        scan_started: selected,
        scanned_messages: if selected { 100 } else { 0 },
        skipped_protected: if selected { 2 } else { 0 },
        next_offset: selected.then_some(9001),
        ..Default::default()
    }
}
