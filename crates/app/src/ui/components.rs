use gpui::{Div, ElementId, Role, SharedString, Stateful, div, prelude::*, px};

use super::{icons::Icon, theme::Theme};

/// Shared mouse, keyboard-focus and accessibility styling, without a UI kit.
fn control(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
) -> Stateful<Div> {
    let theme = Theme::dark();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.into())
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .gap_2()
        .border_1()
        .border_color(theme.border)
        .rounded_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(if primary { theme.text } else { theme.selected }))
        .focus(move |style| style.border_color(if primary { theme.text } else { theme.accent }))
        .active(move |style| style.bg(if primary { theme.text } else { theme.selected }))
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    let label = label.into();
    control(id, label.clone(), false)
        .min_h(px(34.))
        .px_3()
        .py_1()
        .child(label)
}

/// A labeled, keyboard-focusable switch. Thumb position conveys state in
/// addition to color; callers own the value and the activation handlers.
pub fn switch(id: impl Into<ElementId>, label: &'static str, checked: bool) -> Stateful<Div> {
    let theme = Theme::dark();
    div()
        .id(id)
        .role(Role::Switch)
        .aria_label(label)
        .aria_toggled(if checked {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .gap_3()
        .min_h(px(34.))
        .px_1()
        .border_2()
        .border_color(gpui::transparent_black())
        .rounded_md()
        .cursor_pointer()
        .focus(move |style| style.border_color(theme.accent))
        .child(label)
        .child(
            div()
                .relative()
                .w(px(44.))
                .h(px(26.))
                .flex_shrink_0()
                .rounded_full()
                .bg(if checked {
                    theme.switch_on
                } else {
                    theme.switch_off
                })
                .child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .left(px(if checked { 20. } else { 2. }))
                        .size(px(22.))
                        .rounded_full()
                        .bg(theme.switch_thumb),
                ),
        )
}

pub fn primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    let theme = Theme::dark();
    let label = label.into();
    control(id, label.clone(), true)
        .min_h(px(34.))
        .px_3()
        .py_1()
        .child(label)
        .bg(theme.accent)
        .text_color(theme.canvas)
        .border_color(theme.accent)
}

pub fn checkbox(
    id: impl Into<ElementId>,
    label: &'static str,
    checked: bool,
    partial: bool,
) -> Stateful<Div> {
    let theme = Theme::dark();
    control(id, label, false)
        .size(px(20.))
        .flex_shrink_0()
        .justify_center()
        .border_color(theme.muted)
        .when(checked || partial, |mark| {
            mark.bg(theme.accent).child(
                if partial { Icon::Minimize } else { Icon::Check }
                    .element()
                    .text_color(theme.canvas),
            )
        })
}

pub fn format_badge(format: Option<tgbooks_core::BookFormat>) -> Div {
    let theme = Theme::dark();
    div()
        .w(px(50.))
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded_sm()
        .text_size(px(11.))
        .bg(theme.sidebar)
        .text_color(match format {
            Some(tgbooks_core::BookFormat::Pdf) => theme.warning,
            Some(tgbooks_core::BookFormat::Fb2) => theme.success,
            _ => theme.accent,
        })
        .child(format.map_or("Файл".into(), |format| {
            format.extension().to_uppercase()
        }))
}

pub fn navigation_button(
    id: impl Into<ElementId>,
    label: &'static str,
    icon: Icon,
) -> Stateful<Div> {
    control(id, label, false)
        .w_full()
        .min_h(px(36.))
        .px_2()
        .py_1()
        .border_color(Theme::dark().sidebar)
        .child(icon.element())
        .child(label)
}

pub fn icon_button(id: impl Into<ElementId>, label: &'static str, icon: Icon) -> Stateful<Div> {
    control(id, label, false)
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
