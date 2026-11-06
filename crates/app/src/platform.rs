//! UI platform choices; business logic never needs target_os branches.

use gpui::{Bounds, Pixels, TitlebarOptions, WindowOptions, point, px, size};

#[cfg(target_os = "macos")]
pub(crate) mod install;

pub const APP_ID: &str = "org.tgbooks.TelegramBookDownloader";
pub const APP_NAME: &str = "Telegram Books";

pub fn notification_support(cx: &gpui::App) -> bool {
    if cfg!(target_os = "macos") {
        // Call only with the real platform during startup (test/headless
        // platforms do not implement app_path). GPUI also guards the native API.
        cx.app_path()
            .ok()
            .is_some_and(|path| path.extension().is_some_and(|ext| ext == "app"))
    } else {
        cfg!(target_os = "windows")
    }
}

pub fn window_options(bounds: Bounds<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(760.), px(520.))),
        // Full-size content and real native traffic lights, not a borderless
        // imitation that loses close/minimize/fullscreen or accessibility.
        titlebar: Some(TitlebarOptions {
            title: Some("Telegram Books".into()),
            appears_transparent: true,
            traffic_light_position: cfg!(target_os = "macos").then(|| point(px(12.), px(15.))),
        }),
        app_owns_titlebar_drag: cfg!(target_os = "macos"),
        ..Default::default()
    }
}

pub fn font_family() -> &'static str {
    if cfg!(target_os = "macos") {
        ".SystemUIFont"
    } else {
        "Segoe UI"
    }
}

pub fn shortcut(macos: &'static str, windows: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        macos
    } else {
        windows
    }
}

pub fn reveal_file(path: &str, cx: &mut gpui::App) {
    cx.reveal_path(std::path::Path::new(path));
}

pub fn open_file(path: &str, cx: &mut gpui::App) {
    cx.open_with_system(std::path::Path::new(path));
}
