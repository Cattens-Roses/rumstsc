//! Reference-counted bitmap cursors for native pointer presentation.
use crate::CursorStyle;
use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static CURSORS: OnceLock<Mutex<BTreeMap<u64, Weak<CustomCursor>>>> = OnceLock::new();
fn cursors() -> &'static Mutex<BTreeMap<u64, Weak<CustomCursor>>> {
    CURSORS.get_or_init(Default::default)
}
/// An immutable bitmap cursor. Keep this handle alive while using its style.
/// Windows owns an HCURSOR; X11/Wayland use compositor/server cursor resources.
#[derive(Debug)]
pub struct CustomCursor {
    id: u64,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) hot_x: u16,
    pub(crate) hot_y: u16,
    pub(crate) bgra: Vec<u8>,
    #[cfg(target_os = "windows")]
    pub(crate) invert: Vec<bool>,
    #[cfg(target_os = "windows")]
    native_cursor: usize,
}
impl CustomCursor {
    /// Build from straight-alpha, top-down BGRA and per-pixel monochrome XOR
    /// flags. Sizes are limited to 256x256; the hotspot must be inside the image.
    /// Screen XOR is preserved on Windows; Linux uses the supplied BGRA fallback.
    pub fn new(
        width: u16,
        height: u16,
        hot_x: u16,
        hot_y: u16,
        bgra: Vec<u8>,
        invert: Vec<bool>,
    ) -> Result<Arc<Self>> {
        ensure!(
            width > 0 && height > 0 && width <= 256 && height <= 256,
            "Custom cursor dimensions"
        );
        ensure!(hot_x < width && hot_y < height, "Custom cursor hotspot");
        let count = width as usize * height as usize;
        ensure!(
            bgra.len() == count * 4 && invert.len() == count,
            "Custom cursor pixel length"
        );
        let mut cursor = Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            hot_x,
            hot_y,
            bgra,
            #[cfg(target_os = "windows")]
            invert,
            #[cfg(target_os = "windows")]
            native_cursor: 0,
        };
        #[cfg(target_os = "windows")]
        {
            cursor.native_cursor = crate::platform::create_custom_cursor(&cursor)?;
        }
        let cursor = Arc::new(cursor);
        let mut registry = cursors().lock().unwrap();
        registry.retain(|_, cursor| cursor.strong_count() != 0);
        registry.insert(cursor.id, Arc::downgrade(&cursor));
        Ok(cursor)
    }
    /// Use with the normal `.cursor(...)` hitbox style. Mouse motion does not
    /// need any application redraw while this cursor is selected.
    pub fn style(&self) -> CursorStyle {
        CursorStyle::Custom(self.id)
    }
    pub(crate) fn lookup(id: u64) -> Option<Arc<Self>> {
        cursors().lock().unwrap().get(&id).and_then(Weak::upgrade)
    }
    #[cfg(target_os = "windows")]
    pub(crate) fn native_handle(&self) -> usize {
        self.native_cursor
    }
    #[cfg(target_os = "windows")]
    pub(crate) fn from_native(handle: usize) -> Option<Arc<Self>> {
        let all: Vec<_> = cursors()
            .lock()
            .unwrap()
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        all.into_iter()
            .find(|cursor| cursor.native_cursor == handle && handle != 0)
    }
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
    pub(crate) fn premultiplied(&self, scale: u16) -> Vec<u8> {
        let scale = scale.clamp(1, 8) as usize;
        let mut pixels = Vec::with_capacity(self.bgra.len() * scale * scale);
        for y in 0..self.height as usize * scale {
            for x in 0..self.width as usize * scale {
                let i = ((y / scale) * self.width as usize + x / scale) * 4;
                let a = self.bgra[i + 3] as u16;
                let pixel = [
                    (self.bgra[i] as u16 * a / 255) as u8,
                    (self.bgra[i + 1] as u16 * a / 255) as u8,
                    (self.bgra[i + 2] as u16 * a / 255) as u8,
                    a as u8,
                ];
                pixels.extend_from_slice(&u32::from_le_bytes(pixel).to_ne_bytes());
            }
        }
        pixels
    }
}
#[cfg(target_os = "windows")]
impl Drop for CustomCursor {
    fn drop(&mut self) {
        if self.native_cursor != 0 {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyCursor(
                    windows::Win32::UI::WindowsAndMessaging::HCURSOR(self.native_cursor as *mut _),
                );
            }
        }
    }
}
