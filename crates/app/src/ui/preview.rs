//! Opt-in development previews, rendered with the same GPUI tree and native
//! fonts as the application. No desktop capture or Telegram account is needed.

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use gpui::{HeadlessAppContext, prelude::*, px, size};
use tgbooks_core::{AppInfo, CoreEvent};

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
    app.update(&mut cx, |app, _| {
        app.apply_event(CoreEvent::Ready(info.clone()));
        app.apply_event(CoreEvent::HistoryLoaded(Vec::new()));
    });
    for (index, screen) in Screen::ALL.into_iter().enumerate() {
        app.update(&mut cx, |app, cx| app.navigate(screen, cx));
        cx.run_until_parked();
        cx.capture_screenshot(window.into())?
            .save(output.join(format!("{index}-screen.png")))?;
    }
    app.update(&mut cx, |app, cx| {
        app.sidebar_open = false;
        app.navigate(Screen::Library, cx);
    });
    cx.run_until_parked();
    cx.capture_screenshot(window.into())?
        .save(output.join("sidebar-hidden.png"))?;

    let (ui, _compact_backend) = tgbooks_core::connect();
    let compact = cx.open_window(size(px(760.), px(520.)), |_, cx| {
        cx.new(|cx| BookApp::new(ui, cx))
    })?;
    let compact_app = compact.root(&mut cx)?;
    compact_app.update(&mut cx, |app, cx| {
        app.apply_event(CoreEvent::Ready(info));
        app.apply_event(CoreEvent::HistoryLoaded(Vec::new()));
        app.navigate(Screen::Settings, cx);
    });
    cx.run_until_parked();
    cx.capture_screenshot(compact.into())?
        .save(output.join("compact-settings.png"))?;
    println!("UI previews: {}", output.display());
    Ok(())
}
