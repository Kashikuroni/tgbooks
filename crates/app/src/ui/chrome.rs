use gpui::{Div, MouseButton, Window, WindowControlArea, div, prelude::*, px};

use super::{
    components::icon_button,
    icons::Icon,
    theme::{TOOLBAR_HEIGHT, Theme},
};

/// Native macOS controls are overlaid by AppKit; reserve their hit area.
pub fn control_inset() -> f32 {
    if cfg!(target_os = "macos") { 86. } else { 8. }
}

/// Dragging is confined to blank chrome, never navigation or other buttons.
pub fn drag_region() -> impl IntoElement {
    div()
        .id("window-drag-region")
        .h_full()
        .flex_1()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            // Windows handles caption dragging/double-click through non-client
            // hit testing. AppKit needs an explicit native drag operation.
            if !cfg!(target_os = "windows") {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            }
        })
}

pub fn window_controls() -> Div {
    if cfg!(target_os = "windows") {
        caption_controls()
    } else {
        div().flex().h_full().flex_shrink_0()
    }
}

fn caption_controls() -> Div {
    let controls = div().flex().h_full().flex_shrink_0();
    // These GPUI APIs are platform-neutral. Keep this code type-checked on
    // macOS too, even though only Windows displays the custom caption controls.
    let button = |id, label, icon, area| {
        icon_button(id, label, icon)
            // Match native caption buttons: Alt+Space / Alt+F4 provide keyboard
            // access; normal Tab navigation stays in the application content.
            .tab_stop(false)
            .w(px(46.))
            .h_full()
            .rounded_none()
            .window_control_area(area)
            .on_click(move |_, window, _| activate_control(area, window))
            .on_key_down(move |event, window, cx| {
                if super::is_activation_key(event) {
                    activate_control(area, window);
                    cx.stop_propagation();
                }
            })
    };
    controls
        .child(button(
            "window-minimize",
            "Свернуть окно",
            Icon::Minimize,
            WindowControlArea::Min,
        ))
        .child(button(
            "window-maximize",
            "Развернуть или восстановить окно",
            Icon::Maximize,
            WindowControlArea::Max,
        ))
        .child(button(
            "window-close",
            "Закрыть окно",
            Icon::Close,
            WindowControlArea::Close,
        ))
}

fn activate_control(area: WindowControlArea, window: &mut Window) {
    match area {
        WindowControlArea::Min => window.minimize_window(),
        WindowControlArea::Max => window.zoom_window(),
        WindowControlArea::Close => window.remove_window(),
        WindowControlArea::Drag => {}
    }
}

pub fn toolbar(toggle: impl IntoElement) -> Div {
    let theme = Theme::dark();
    div()
        .flex()
        .items_center()
        .h(px(TOOLBAR_HEIGHT))
        .flex_shrink_0()
        .bg(theme.sidebar)
        .border_b_1()
        .border_color(theme.border)
        .child(div().w(px(control_inset())).flex_shrink_0())
        .child(toggle)
        .child(drag_region())
        .child(window_controls())
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use super::*;
    use gpui::{Context, FocusHandle, Render, TestAppContext, VisualTestContext};

    struct TabOrder {
        before: FocusHandle,
        after: FocusHandle,
    }

    impl Render for TabOrder {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(div().id("before-caption").track_focus(&self.before))
                // Render Windows controls even on macOS to catch focus-order
                // regressions without calling TestWindow's unsupported minimize.
                .child(caption_controls())
                .child(div().id("after-caption").track_focus(&self.after))
        }
    }

    #[gpui::test]
    fn caption_buttons_do_not_intercept_content_tab_navigation(cx: &mut TestAppContext) {
        let handle = cx.open_window(gpui::size(px(760.), px(520.)), |window, cx| {
            let view = TabOrder {
                before: cx.focus_handle().tab_stop(true),
                after: cx.focus_handle().tab_stop(true),
            };
            view.before.focus(window, cx);
            view
        });
        let Ok(view) = handle.root(cx) else {
            panic!("test window missing");
        };
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.focus_next(cx);
            assert!(view.read(cx).after.is_focused(window));
            window.focus_prev(cx);
            assert!(view.read(cx).before.is_focused(window));
        });
    }
}
