//! UI platform choices; business logic never needs target_os branches.

use gpui::{Bounds, Pixels, TitlebarOptions, WindowOptions, point, px, size};

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
