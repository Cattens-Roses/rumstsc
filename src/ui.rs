use crate::input::TextField;
use crate::theme::Palette;
use gpui::{prelude::*, *};

pub fn choice(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    p: Palette,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .h(px(36.))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .border_color(if selected { p.accent } else { p.border })
        .bg(if selected { p.soft } else { p.panel })
        .text_color(if selected { p.accent } else { p.text })
        .text_sm()
        .hover(|s| s.bg(p.soft))
        .cursor_pointer()
        .child(label)
}
pub fn section(title: &'static str, description: &'static str, p: Palette) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_5()
        .p_5()
        .rounded(px(12.))
        .bg(p.panel)
        .border_1()
        .border_color(p.border)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_base()
                        .child(title),
                )
                .child(div().text_xs().text_color(p.muted).child(description)),
        )
}
pub fn setting(
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
    p: Palette,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_xs().text_color(p.muted).child(description)),
        )
        .child(control)
}
pub fn desktop_mark(ink: Hsla, background: Hsla, size: f32) -> impl IntoElement {
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size * 0.25))
        .bg(background)
        .child(
            svg()
                .path("rdpicon.svg")
                .size(px(size * 0.8))
                .text_color(ink),
        )
}
pub fn field(label: &'static str, input: Entity<TextField>, muted: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .child(label),
        )
        .child(input)
}
