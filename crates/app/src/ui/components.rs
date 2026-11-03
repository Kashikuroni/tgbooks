use gpui::{Div, ElementId, Role, SharedString, Stateful, div, prelude::*, px};

use super::{icons::Icon, theme::Theme};

/// Shared mouse, keyboard-focus and accessibility styling, without a UI kit.
fn control(id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
    let theme = Theme::dark();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .gap_2()
        .border_1()
        .border_color(theme.border)
        .rounded_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(theme.selected))
        .focus(move |style| style.border_color(theme.accent))
        .active(move |style| style.bg(theme.selected))
}

pub fn button(id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
    control(id, label).min_h(px(34.)).px_3().py_1().child(label)
}

pub fn navigation_button(
    id: impl Into<ElementId>,
    label: &'static str,
    icon: Icon,
) -> Stateful<Div> {
    control(id, label)
        .w_full()
        .min_h(px(36.))
        .px_2()
        .py_1()
        .border_color(Theme::dark().sidebar)
        .child(icon.element())
        .child(label)
}

pub fn icon_button(id: impl Into<ElementId>, label: &'static str, icon: Icon) -> Stateful<Div> {
    control(id, label)
        .justify_center()
        .size(px(32.))
        .border_color(Theme::dark().sidebar)
        .child(icon.element())
}

pub fn empty_state(title: &'static str, description: &'static str) -> Div {
    let theme = Theme::dark();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .max_w(px(600.))
        .py_8()
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(title),
        )
        .child(div().text_color(theme.muted).child(description))
}

pub fn setting(label: &'static str, value: impl Into<SharedString>) -> Div {
    let theme = Theme::dark();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .py_4()
        .border_b_1()
        .border_color(theme.border)
        .child(div().font_weight(gpui::FontWeight::MEDIUM).child(label))
        .child(div().text_color(theme.muted).child(value.into()))
}
