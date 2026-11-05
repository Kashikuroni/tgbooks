// Portions derived from GPUI examples/input.rs.
// Copyright 2022 - 2025 Zed Industries, Inc.
// Licensed under Apache-2.0; see ../../LICENSE-APACHE and ../../NOTICE.
// Modified for Telegram Books: zeroized buffers, masking, bounded single-line
// input, horizontal caret scrolling, clipboard policy and accessibility roles.
//
//! Single-line GPUI editor using the official EntityInputHandler contract.
//! Text shaping/IME structure follows Zed's GPUI `examples/input.rs`; unlike
//! that demonstration, secrets are zeroized, masked in every display/IME path,
//! and never copied to the clipboard.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point, Role, ShapedLine, SharedString,
    Style, TextRun, UTF16Selection, Window, actions, div, fill, point, prelude::*, px, relative,
    size,
};
use tgbooks_core::SecretString;
use unicode_segmentation::UnicodeSegmentation as _;
use zeroize::{Zeroize as _, Zeroizing};

use super::theme::Theme;

actions!(
    auth_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Copy,
        Cut
    ]
);

pub fn bind_keys(cx: &mut App) {
    use crate::platform::shortcut;
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("AuthInput")),
        KeyBinding::new("delete", Delete, Some("AuthInput")),
        KeyBinding::new("left", Left, Some("AuthInput")),
        KeyBinding::new("right", Right, Some("AuthInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("AuthInput")),
        KeyBinding::new("shift-right", SelectRight, Some("AuthInput")),
        KeyBinding::new("home", Home, Some("AuthInput")),
        KeyBinding::new("end", End, Some("AuthInput")),
        KeyBinding::new(shortcut("cmd-a", "ctrl-a"), SelectAll, Some("AuthInput")),
        KeyBinding::new(shortcut("cmd-v", "ctrl-v"), Paste, Some("AuthInput")),
        KeyBinding::new(shortcut("cmd-c", "ctrl-c"), Copy, Some("AuthInput")),
        KeyBinding::new(shortcut("cmd-x", "ctrl-x"), Cut, Some("AuthInput")),
    ]);
}

pub struct TextInput {
    pub focus: FocusHandle,
    value: Zeroizing<String>,
    label: &'static str,
    placeholder: &'static str,
    masked: bool,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    selecting: bool,
    layout: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    scroll: Pixels,
}

impl TextInput {
    pub fn new(
        label: &'static str,
        placeholder: &'static str,
        masked: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle().tab_stop(true),
            value: Zeroizing::new(String::new()),
            label,
            placeholder,
            masked,
            selection: 0..0,
            reversed: false,
            marked: None,
            selecting: false,
            layout: None,
            bounds: None,
            scroll: px(0.),
        }
    }

    pub fn secret(&self) -> SecretString {
        SecretString::new(self.value.to_string())
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.value.zeroize();
        self.selection = 0..0;
        self.reversed = false;
        self.marked = None;
        self.layout = None;
        self.scroll = px(0.);
        cx.notify();
    }

    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    fn utf8(&self, utf16: usize) -> usize {
        let mut count = 0;
        for (index, ch) in self.value.char_indices() {
            if count >= utf16 {
                return index;
            }
            count += ch.len_utf16();
        }
        self.value.len()
    }

    fn utf16(&self, utf8: usize) -> usize {
        self.value
            .char_indices()
            .take_while(|(index, _)| *index < utf8)
            .map(|(_, ch)| ch.len_utf16())
            .sum()
    }

    fn range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        let a = self.utf8(range.start);
        let b = self.utf8(range.end);
        a.min(b)..a.max(b)
    }

    fn to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.utf16(range.start)..self.utf16(range.end)
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn display(&self) -> SharedString {
        if self.masked {
            "•".repeat(self.value.encode_utf16().count()).into()
        } else {
            self.value.to_string().into()
        }
    }

    fn display_index(&self, index: usize) -> usize {
        if self.masked {
            self.utf16(index) * "•".len()
        } else {
            index
        }
    }

    fn previous(&self) -> usize {
        self.value
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < self.cursor()).then_some(index))
            .unwrap_or(0)
    }

    fn next(&self) -> usize {
        self.value
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > self.cursor()).then_some(index))
            .unwrap_or(self.value.len())
    }

    fn move_cursor(
        &mut self,
        offset: usize,
        select: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if select {
            let anchor = if self.reversed {
                self.selection.end
            } else {
                self.selection.start
            };
            self.selection = anchor.min(offset)..anchor.max(offset);
            self.reversed = offset < anchor;
        } else {
            self.selection = offset..offset;
            self.reversed = false;
        }
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }

    fn mouse_index(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.bounds, self.layout.as_ref()) else {
            return 0;
        };
        if self.value.is_empty() || line.text != self.display() {
            return 0;
        }
        let index = line.closest_index_for_x(position.x - bounds.left() + self.scroll);
        if self.masked {
            self.utf8(index / "•".len())
        } else {
            index.min(self.value.len())
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.selecting = true;
        self.move_cursor(
            self.mouse_index(event.position),
            event.modifiers.shift,
            window,
            cx,
        );
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.move_cursor(self.mouse_index(event.position), true, window, cx);
        }
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.selection.is_empty() {
            self.previous()
        } else {
            self.selection.start
        };
        self.move_cursor(next, false, window, cx);
    }
    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.selection.is_empty() {
            self.next()
        } else {
            self.selection.end
        };
        self.move_cursor(next, false, window, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(self.previous(), true, window, cx);
    }
    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(self.next(), true, window, cx);
    }
    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(0, false, window, cx);
    }
    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(self.value.len(), false, window, cx);
    }
    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(0, false, window, cx);
        self.move_cursor(self.value.len(), true, window, cx);
    }
    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_cursor(self.previous(), true, window, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_cursor(self.next(), true, window, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(value) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let value = Zeroizing::new(value);
            self.replace_text_in_range(None, &value, window, cx);
        }
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.value[self.selection.clone()].to_owned(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.masked {
            self.copy(&Copy, window, cx);
            self.replace_text_in_range(None, "", window, cx);
        }
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(range);
        let actual_range = self.to_utf16(range.clone());
        let length = actual_range.len();
        *actual = Some(actual_range);
        Some(if self.masked {
            "•".repeat(length)
        } else {
            self.value[range].to_owned()
        })
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.to_utf16(self.selection.clone()),
            reversed: self.reversed,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection = self.range_from_utf16(range);
        self.reversed = false;
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.value.encode_utf16().count())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone().map(|range| self.to_utf16(range))
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_and_mark_text_in_range(range, value, None, window, cx);
        self.unmark_text(window, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        value: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let value = Zeroizing::new(
            value
                .chars()
                .filter(|ch| self.masked || !ch.is_control())
                .take(4096)
                .collect::<String>(),
        );
        if self.value.len() - range.len() + value.len() > 16_384 {
            return;
        }
        self.value.replace_range(range.clone(), &value);
        self.marked = (!value.is_empty()).then_some(range.start..range.start + value.len());
        let start = self.utf16(range.start);
        let length = value.encode_utf16().count();
        self.selection = selection
            .map(|range| {
                self.range_from_utf16(
                    start + range.start.min(length)..start + range.end.min(length),
                )
            })
            .unwrap_or_else(|| range.start + value.len()..range.start + value.len());
        self.reversed = false;
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.layout.as_ref()?;
        if line.text != self.display() {
            return None;
        }
        let range = self.range_from_utf16(range);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(self.display_index(range.start)) - self.scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(self.display_index(range.end)) - self.scroll,
                bounds.bottom(),
            ),
        ))
    }
    fn selection_bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range, bounds, window, cx)
            .into_iter()
            .collect()
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.bounds?.localize(&point)?;
        if self.layout.as_ref()?.text != self.display() {
            return None;
        }
        Some(self.utf16(self.mouse_index(point)))
    }
}

struct TextElement(Entity<TextInput>);
struct PaintState {
    line: ShapedLine,
    caret: PaintQuad,
    selection: Option<PaintQuad>,
    scroll: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PaintState;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(24.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PaintState {
        let input = self.0.read(cx);
        let style = window.text_style();
        let text = if input.value.is_empty() {
            SharedString::from(input.placeholder)
        } else {
            input.display()
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: if input.value.is_empty() {
                Theme::dark().muted
            } else {
                style.color
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            text,
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );
        let cursor = if input.value.is_empty() {
            px(0.)
        } else {
            line.x_for_index(input.display_index(input.cursor()))
        };
        let scroll = if cursor > bounds.size.width - px(2.) {
            cursor - bounds.size.width + px(2.)
        } else {
            px(0.)
        };
        let caret = fill(
            Bounds::new(
                point(bounds.left() + cursor - scroll, bounds.top()),
                size(px(1.), bounds.size.height),
            ),
            Theme::dark().accent,
        );
        let selection = (!input.selection.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left()
                            + line.x_for_index(input.display_index(input.selection.start))
                            - scroll,
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(input.display_index(input.selection.end))
                            - scroll,
                        bounds.bottom(),
                    ),
                ),
                Theme::dark().selected,
            )
        });
        PaintState {
            line,
            caret,
            selection,
            scroll,
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut PaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.0.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        if let Some(selection) = state.selection.take() {
            window.paint_quad(selection);
        }
        let _ = state.line.paint(
            point(bounds.left() - state.scroll, bounds.top()),
            px(24.),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        );
        if focus.is_focused(window) {
            window.paint_quad(state.caret.clone());
        }
        self.0.update(cx, |input, _| {
            input.layout = Some(state.line.clone());
            input.bounds = Some(bounds);
            input.scroll = state.scroll;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        div()
            .id("auth-input")
            .role(if self.masked {
                Role::PasswordInput
            } else {
                Role::TextInput
            })
            .aria_label(self.label)
            .aria_placeholder(self.placeholder)
            .aria_value(self.display())
            .key_context("AuthInput")
            .track_focus(&self.focus)
            .w_full()
            .h(px(42.))
            .p_2()
            .border_1()
            .border_color(theme.border)
            .rounded_sm()
            .bg(theme.canvas)
            .cursor(CursorStyle::IBeam)
            .overflow_hidden()
            .focus(move |style| style.border_color(theme.accent))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|input, _, _, _| input.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|input, _, _, _| input.selecting = false),
            )
            .child(TextElement(cx.entity()))
    }
}

#[cfg(all(test, feature = "gpui-tests"))]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn masked_input_never_exposes_plaintext_to_ime_or_clipboard(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("Пароль", "", true, cx));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "é🙂漢", window, cx);
                assert_eq!(input.display().as_ref(), "••••");
                let mut actual = None;
                assert_eq!(
                    input.text_for_range(1..3, &mut actual, window, cx),
                    Some("••".into())
                );
                assert_eq!(actual, Some(1..3));
                input.select_all(&SelectAll, window, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("untouched".into()));
                input.copy(&Copy, window, cx);
                input.cut(&Cut, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("untouched".into())
                );
                assert_eq!(input.secret().expose(), "é🙂漢");
                cx.write_to_clipboard(ClipboardItem::new_string("another dummy password".into()));
                input.paste(&Paste, window, cx);
                assert_eq!(input.secret().expose(), "another dummy password");
                input.clear(cx);
                assert!(input.secret().expose().is_empty());
                assert!(input.layout.is_none());
            });
        });
    }

    #[gpui::test]
    fn unicode_selection_composition_and_deletion_preserve_boundaries(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("Input", "", false, cx));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "é🙂漢", window, cx);
                assert_eq!(input.text_length_utf16(window, cx), Some(4));
                input.set_selected_text_range(1..3, window, cx);
                assert_eq!(input.selection, 2..6);
                input.replace_and_mark_text_in_range(None, "e\u{301}🙂", Some(2..4), window, cx);
                assert_eq!(input.secret().expose(), "ée\u{301}🙂漢");
                assert_eq!(input.selection, 5..9);
                assert_eq!(
                    input
                        .selected_text_range(false, window, cx)
                        .map(|selection| selection.range),
                    Some(3..5)
                );
                input.unmark_text(window, cx);
                input.delete(&Delete, window, cx);
                input.backspace(&Backspace, window, cx);
                assert_eq!(input.secret().expose(), "é漢");
            });
        });
    }

    #[gpui::test]
    fn long_input_scrolls_to_keep_the_caret_inside_the_field(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("Input", "", false, cx));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.focus.focus(window, cx);
                input.replace_text_in_range(None, &"x".repeat(2000), window, cx);
            });
        });
        cx.run_until_parked();
        input.read_with(&cx.cx, |input, _| assert!(input.scroll > px(0.)));
    }
}
