use gpui::{Hsla, rgb};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Hsla,
    pub panel: Hsla,
    pub field: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub soft: Hsla,
    pub error: Hsla,
}
impl Theme {
    pub fn palette(self) -> Palette {
        let (bg, panel, field, border, text, muted, accent, soft, error) = match self {
            Self::Dark => (
                0x101216, 0x191c22, 0x12151a, 0x303640, 0xedf0f6, 0x9ca6b7, 0xa6b7ff, 0x29334e,
                0xffa6ad,
            ),
            Self::Light => (
                0xf1f3f8, 0xffffff, 0xf8f9fc, 0xdce1eb, 0x202737, 0x647086, 0x465fca, 0xe9edfc,
                0xb52e46,
            ),
        };
        Palette {
            bg: rgb(bg).into(),
            panel: rgb(panel).into(),
            field: rgb(field).into(),
            border: rgb(border).into(),
            text: rgb(text).into(),
            muted: rgb(muted).into(),
            accent: rgb(accent).into(),
            soft: rgb(soft).into(),
            error: rgb(error).into(),
        }
    }
    pub fn toggle(&mut self) {
        *self = if *self == Self::Dark {
            Self::Light
        } else {
            Self::Dark
        };
    }
}
