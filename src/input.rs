use crate::theme::Theme;
use gpui::{
    Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, ShapedLine,
    SharedString, TextRun, UTF16Selection, Window, canvas, div, fill, point, prelude::*, px, size,
};
use std::ops::Range;
use zeroize::Zeroize;

pub struct TextField {
    pub value: String,
    pub focus: FocusHandle,
    pub theme: Theme,
    pub secret: bool,
    placeholder: &'static str,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    line: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    offset: Pixels,
}
impl Drop for TextField {
    fn drop(&mut self) {
        if self.secret {
            self.value.zeroize();
        }
    }
}
impl TextField {
    pub fn new(
        value: String,
        placeholder: &'static str,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Self {
        let end = value.len();
        Self {
            value,
            focus: cx.focus_handle(),
            theme,
            secret: false,
            placeholder,
            selection: end..end,
            marked: None,
            line: None,
            bounds: None,
            offset: px(0.),
        }
    }
    pub fn set(&mut self, value: String, cx: &mut Context<Self>) {
        if self.secret {
            self.value.zeroize();
        }
        self.value = value;
        self.selection = self.value.len()..self.value.len();
        self.marked = None;
        cx.notify();
    }
    fn byte(&self, units: usize) -> usize {
        byte_offset(&self.value, units)
    }
    fn visual(&self, byte: usize) -> usize {
        if self.secret {
            self.value[..byte].chars().count() * 3
        } else {
            byte
        }
    }
    fn actual(&self, byte: usize) -> usize {
        if self.secret {
            self.value
                .char_indices()
                .nth(byte / 3)
                .map(|(i, _)| i)
                .unwrap_or(self.value.len())
        } else {
            byte.min(self.value.len())
        }
    }
    fn units(&self, byte: usize) -> usize {
        self.value[..byte].encode_utf16().count()
    }
    fn range(&self, r: Range<usize>) -> Range<usize> {
        self.byte(r.start)..self.byte(r.end)
    }
    fn replace(&mut self, r: Option<Range<usize>>, text: &str, cx: &mut Context<Self>) {
        let r = r
            .map(|r| self.range(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        let text: String = text.chars().filter(|c| !c.is_control()).take(256).collect();
        if self.value.len() - (r.end - r.start) + text.len() > 1024 {
            return;
        }
        self.value.replace_range(r.clone(), &text);
        let end = r.start + text.len();
        self.selection = end..end;
        self.marked = None;
        cx.notify();
    }
    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = e.keystroke.key.as_str();
        let modifiers = e.keystroke.modifiers;
        if modifiers.control || modifiers.platform {
            match key {
                "a" => self.selection = 0..self.value.len(),
                "v" => {
                    if let Some(s) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.replace(None, &s, cx);
                    }
                }
                "c" | "x" => {
                    if self.secret {
                        return;
                    }
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.value[self.selection.clone()].into(),
                    ));
                    if key == "x" {
                        self.replace(None, "", cx);
                    }
                }
                _ => return,
            }
        } else {
            let prev = self.value[..self.selection.start]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            let next = self.value[self.selection.end..]
                .chars()
                .next()
                .map(|c| self.selection.end + c.len_utf8())
                .unwrap_or(self.value.len());
            match key {
                "backspace" => {
                    if self.selection.is_empty() {
                        self.selection.start = prev;
                    }
                    self.replace(None, "", cx);
                }
                "delete" => {
                    if self.selection.is_empty() {
                        self.selection.end = next;
                    }
                    self.replace(None, "", cx);
                }
                "left" => {
                    if modifiers.shift {
                        self.selection.start = prev;
                    } else {
                        let p = if self.selection.is_empty() {
                            prev
                        } else {
                            self.selection.start
                        };
                        self.selection = p..p;
                    }
                }
                "right" => {
                    if modifiers.shift {
                        self.selection.end = next;
                    } else {
                        let p = if self.selection.is_empty() {
                            next
                        } else {
                            self.selection.end
                        };
                        self.selection = p..p;
                    }
                }
                "home" => {
                    if modifiers.shift {
                        self.selection.start = 0;
                    } else {
                        self.selection = 0..0;
                    }
                }
                "end" => {
                    let n = self.value.len();
                    if modifiers.shift {
                        self.selection.end = n;
                    } else {
                        self.selection = n..n;
                    }
                }
                _ => return,
            }
        }
        self.marked = None;
        window.refresh();
        cx.notify();
        cx.stop_propagation();
    }
}
fn byte_offset(s: &str, units: usize) -> usize {
    let mut count = 0;
    for (i, ch) in s.char_indices() {
        if count + ch.len_utf16() > units {
            return i;
        }
        count += ch.len_utf16();
    }
    s.len()
}
impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        r: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.range(r);
        *actual = Some(self.units(r.start)..self.units(r.end));
        if self.secret {
            None
        } else {
            Some(self.value[r].into())
        }
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.units(self.selection.start)..self.units(self.selection.end),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|r| self.units(r.start)..self.units(r.end))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        r: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace(r, text, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        r: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = r
            .as_ref()
            .map(|r| self.byte(r.start))
            .or_else(|| self.marked.as_ref().map(|r| r.start))
            .unwrap_or(self.selection.start);
        self.replace(r, text, cx);
        let end = self.selection.end;
        if end > start {
            self.marked = Some(start..end);
            if let Some(r) = selected {
                let s = &self.value[start..end];
                self.selection = start + byte_offset(s, r.start)..start + byte_offset(s, r.end);
            }
        }
    }
    fn bounds_for_range(
        &mut self,
        r: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let r = self.range(r);
        let line = self.line.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(self.visual(r.start)) - self.offset,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(self.visual(r.end)) - self.offset,
                bounds.bottom(),
            ),
        ))
    }
    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let line = self.line.as_ref()?;
        let b = self.bounds?;
        Some(self.units(self.actual(line.closest_index_for_x(p.x - b.left() + self.offset))))
    }
}
impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.theme.palette();
        let focus = self.focus.is_focused(window);
        let entity = cx.entity();
        let paint_entity = entity.clone();
        div()
            .id("field")
            .w_full()
            .h(px(44.))
            .px_3()
            .flex()
            .items_center()
            .overflow_hidden()
            .track_focus(&self.focus)
            .bg(palette.field)
            .border_1()
            .border_color(if focus {
                palette.accent
            } else {
                palette.border
            })
            .rounded(px(8.))
            .hover(|s| s.border_color(palette.accent))
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    this.focus.focus(window);
                    if let (Some(line), Some(b)) = (&this.line, this.bounds) {
                        let i = this.actual(
                            line.closest_index_for_x(e.position.x - b.left() + this.offset),
                        );
                        this.selection = if e.click_count >= 2 {
                            0..this.value.len()
                        } else {
                            i..i
                        };
                    }
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    move |bounds, window, cx| {
                        entity.update(cx, |this, _| {
                            let text: SharedString = if this.value.is_empty() {
                                this.placeholder.into()
                            } else if this.secret {
                                "•".repeat(this.value.chars().count()).into()
                            } else {
                                this.value.clone().into()
                            };
                            let run = TextRun {
                                len: text.len(),
                                font: window.text_style().font(),
                                color: if this.value.is_empty() {
                                    palette.muted
                                } else {
                                    palette.text
                                },
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let line = window.text_system().shape_line(text, px(14.), &[run], None);
                            let selection =
                                this.visual(this.selection.start)..this.visual(this.selection.end);
                            let caret = line.x_for_index(selection.end);
                            this.offset = (caret - bounds.size.width + px(2.)).max(px(0.));
                            (line, selection, this.offset, this.focus.clone())
                        })
                    },
                    move |bounds, (line, selection, offset, focus), window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, paint_entity.clone()),
                            cx,
                        );
                        let origin = point(bounds.left() - offset, bounds.top());
                        if focus.is_focused(window) {
                            if selection.is_empty() {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(
                                            origin.x + line.x_for_index(selection.end),
                                            bounds.top() + px(2.),
                                        ),
                                        size(px(1.), px(18.)),
                                    ),
                                    palette.accent,
                                ));
                            } else {
                                window.paint_quad(fill(
                                    Bounds::from_corners(
                                        point(
                                            origin.x + line.x_for_index(selection.start),
                                            bounds.top(),
                                        ),
                                        point(
                                            origin.x + line.x_for_index(selection.end),
                                            bounds.bottom(),
                                        ),
                                    ),
                                    palette.soft,
                                ));
                            }
                        }
                        let _ = line.paint(origin, px(22.), window, cx);
                        paint_entity.update(cx, |this, _| {
                            this.line = Some(line);
                            this.bounds = Some(bounds);
                        });
                    },
                )
                .w_full()
                .h(px(22.)),
            )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_boundaries() {
        let s = "a🦀é";
        assert_eq!(byte_offset(s, 0), 0);
        assert_eq!(byte_offset(s, 1), 1);
        assert_eq!(byte_offset(s, 2), 1);
        assert_eq!(byte_offset(s, 3), 5);
        assert_eq!(byte_offset(s, 99), 7);
    }
}
