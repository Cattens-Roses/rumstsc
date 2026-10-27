use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "rdpicon.svg" => Some(Cow::Borrowed(include_bytes!("../rdpicon.svg"))),
            _ => None,
        })
    }
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(if path.is_empty() {
            vec!["rdpicon.svg".into()]
        } else {
            vec![]
        })
    }
}
