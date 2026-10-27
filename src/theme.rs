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
                0x111315, 0x191c1f, 0x121517, 0x32383e, 0xe9edf0, 0x9aa5af, 0x86bfff, 0x25384d,
                0xffa7a7,
            ),
            Self::Light => (
                0xf3f5f7, 0xffffff, 0xf8fafb, 0xd5dce2, 0x17212b, 0x5b6875, 0x225eaf, 0xe8f0fb,
                0xa52b38,
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
