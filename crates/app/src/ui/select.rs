//! Small keyboard-operable select, using GPUI only (no UI kit).

use gpui::{
    Context, FocusHandle, KeyDownEvent, MouseButton, Role, SharedString, div, prelude::*, px,
};

use super::{components::button, icons::Icon, is_activation_key, theme::Theme};

pub struct Select {
    label: &'static str,
    options: Vec<SharedString>,
    pub selected: usize,
    cursor: usize,
    open: bool,
    focus: FocusHandle,
    blur: Option<gpui::Subscription>,
}

impl Select {
    pub fn new(label: &'static str, options: Vec<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            label,
            options,
            selected: 0,
            cursor: 0,
            open: false,
            focus: cx.focus_handle().tab_stop(true),
            blur: None,
        }
    }

    pub fn formats(cx: &mut Context<Self>) -> Self {
        Self::new(
            "Формат",
            std::iter::once("Все".into())
                .chain(
                    tgbooks_core::BookFormat::ALL
                        .map(|format| format.extension().to_uppercase().into()),
                )
                .collect(),
            cx,
        )
    }

    pub fn format(&self) -> Option<tgbooks_core::BookFormat> {
        self.selected
            .checked_sub(1)
            .and_then(|index| tgbooks_core::BookFormat::ALL.get(index).copied())
    }

    fn commit(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index.min(self.options.len().saturating_sub(1));
        self.open = false;
        cx.notify();
    }
}

impl Render for Select {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.blur.is_none() {
            self.blur = Some(cx.on_blur(&self.focus, window, |view, _, cx| {
                if view.open {
                    view.open = false;
                    cx.notify();
                }
            }));
        }
        let theme = Theme::dark();
        let value = self.options.get(self.selected).cloned().unwrap_or_default();
        div()
            .relative()
            .flex_shrink_0()
            .child(
                button(
                    "select-trigger",
                    if self.label.is_empty() {
                        value.to_string()
                    } else {
                        format!("{}: {value}", self.label)
                    },
                )
                .track_focus(&self.focus)
                .aria_expanded(self.open)
                .text_sm()
                .child(Icon::ChevronDown.element())
                .on_click(cx.listener(|view, _, window, cx| {
                    view.focus.focus(window, cx);
                    view.open = !view.open;
                    view.cursor = view.selected;
                    cx.notify();
                }))
                .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                    let key = event.keystroke.key.as_str();
                    if key == "escape" {
                        view.open = false;
                    } else if matches!(key, "down" | "up" | "home" | "end") {
                        view.open = true;
                        view.cursor = match key {
                            "up" => view.cursor.saturating_sub(1),
                            "home" => 0,
                            "end" => view.options.len().saturating_sub(1),
                            _ => (view.cursor + 1).min(view.options.len().saturating_sub(1)),
                        };
                    } else if is_activation_key(event) {
                        if view.open {
                            view.commit(view.cursor, cx);
                        } else {
                            view.open = true;
                            view.cursor = view.selected;
                        }
                    } else if key == "tab" {
                        view.open = false;
                        cx.notify();
                        return;
                    } else {
                        return;
                    }
                    cx.stop_propagation();
                    cx.notify();
                })),
            )
            .when(self.open, |root| {
                root.child(
                    gpui::deferred(
                        div()
                            .id("select-options")
                            .role(Role::ListBox)
                            .aria_label(self.label)
                            .absolute()
                            .top(px(38.))
                            .right_0()
                            .w(px(188.))
                            .p_1()
                            .bg(theme.sidebar)
                            .border_1()
                            .border_color(theme.border)
                            .rounded_md()
                            .shadow_lg()
                            .occlude()
                            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                                view.open = false;
                                cx.notify();
                            }))
                            .children(self.options.iter().enumerate().map(|(index, option)| {
                                div()
                                    .id(("option", index))
                                    .role(Role::ListBoxOption)
                                    .aria_label(option.clone())
                                    .aria_selected(index == self.selected)
                                    .when(index == self.cursor, |row| {
                                        row.aria_active_descendant().bg(theme.selected)
                                    })
                                    .h(px(32.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(theme.selected))
                                    .child(option.clone())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |view, _, _, cx| {
                                            view.commit(index, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                            })),
                    )
                    .with_priority(1),
                )
            })
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn choices_support_commit_escape_and_blur_without_losing_focus(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let view = Select::formats(cx);
            view.focus.focus(window, cx);
            view
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.simulate_keystrokes("enter down down enter");
        view.read_with(&cx.cx, |view, _| {
            assert_eq!(view.format(), Some(tgbooks_core::BookFormat::Epub));
            assert!(!view.open);
        });
        cx.simulate_keystrokes("enter end escape");
        view.read_with(&cx.cx, |view, _| {
            assert_eq!(view.format(), Some(tgbooks_core::BookFormat::Epub));
            assert!(!view.open);
        });
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            window.blur(cx);
        });
        cx.run_until_parked();
        view.read_with(&cx.cx, |view, _| assert!(!view.open));
    }
}
