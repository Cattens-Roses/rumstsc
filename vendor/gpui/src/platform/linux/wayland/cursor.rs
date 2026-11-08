use crate::Globals;
use crate::platform::linux::{DEFAULT_CURSOR_ICON_NAME, log_cursor_icon_warning};
use anyhow::{Context as _, anyhow};
use util::ResultExt;

use wayland_client::Connection;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::protocol::{wl_pointer::WlPointer, wl_shm::WlShm};
use wayland_cursor::{CursorImageBuffer, CursorTheme};

pub(crate) struct Cursor {
    loaded_theme: Option<LoadedTheme>,
    size: u32,
    scaled_size: u32,
    surface: WlSurface,
    shm: WlShm,
    connection: Connection,
    qh: wayland_client::QueueHandle<super::WaylandClientStatePtr>,
    custom_buffer: Option<wayland_client::protocol::wl_buffer::WlBuffer>,
    custom_key: Option<(u64, i32)>,
}

pub(crate) struct LoadedTheme {
    theme: CursorTheme,
    name: Option<String>,
    scaled_size: u32,
}

impl Drop for Cursor {
    fn drop(&mut self) {
        self.loaded_theme.take();
        if let Some(buffer) = self.custom_buffer.take() {
            buffer.destroy();
        }
        self.surface.destroy();
    }
}

impl Cursor {
    pub fn new(connection: &Connection, globals: &Globals, size: u32) -> Self {
        let mut this = Self {
            loaded_theme: None,
            size,
            scaled_size: size,
            surface: globals.compositor.create_surface(&globals.qh, ()),
            shm: globals.shm.clone(),
            connection: connection.clone(),
            qh: globals.qh.clone(),
            custom_buffer: None,
            custom_key: None,
        };
        this.set_theme_internal(None);
        this
    }

    fn set_theme_internal(&mut self, theme_name: Option<String>) {
        if let Some(loaded_theme) = self.loaded_theme.as_ref()
            && loaded_theme.name == theme_name
            && loaded_theme.scaled_size == self.scaled_size
        {
            return;
        }
        let result = if let Some(theme_name) = theme_name.as_ref() {
            CursorTheme::load_from_name(
                &self.connection,
                self.shm.clone(),
                theme_name,
                self.scaled_size,
            )
        } else {
            CursorTheme::load(&self.connection, self.shm.clone(), self.scaled_size)
        };
        if let Some(theme) = result
            .context("Wayland: Failed to load cursor theme")
            .log_err()
        {
            self.loaded_theme = Some(LoadedTheme {
                theme,
                name: theme_name,
                scaled_size: self.scaled_size,
            });
        }
    }

    pub fn set_theme(&mut self, theme_name: String) {
        self.set_theme_internal(Some(theme_name));
    }

    fn set_scaled_size(&mut self, scaled_size: u32) {
        self.scaled_size = scaled_size;
        let theme_name = self
            .loaded_theme
            .as_ref()
            .and_then(|loaded_theme| loaded_theme.name.clone());
        self.set_theme_internal(theme_name);
    }

    pub fn set_size(&mut self, size: u32) {
        self.size = size;
        self.set_scaled_size(size);
    }

    pub fn clear_custom(&mut self) {
        if let Some(buffer) = self.custom_buffer.take() {
            self.surface.attach(None, 0, 0);
            self.surface.commit();
            buffer.destroy();
        }
        self.custom_key = None;
    }
    pub fn custom_matches(&self, id: u64, scale: i32) -> bool {
        self.custom_key == Some((id, scale.clamp(1, 8)))
    }
    pub fn set_custom(
        &mut self,
        pointer: &WlPointer,
        serial: u32,
        image: &crate::CustomCursor,
        scale: i32,
    ) -> anyhow::Result<()> {
        use std::{
            fs::File,
            io::Write,
            os::fd::{AsFd, FromRawFd},
        };
        let scale = scale.clamp(1, 8);
        let id = match image.style() {
            crate::CursorStyle::Custom(id) => id,
            _ => unreachable!(),
        };
        if self.custom_matches(id, scale) {
            pointer.set_cursor(
                serial,
                Some(&self.surface),
                image.hot_x as i32,
                image.hot_y as i32,
            );
            return Ok(());
        }
        // Immutable anonymous backing store. The compositor owns its fd after
        // transfer; it can keep reading after a replaced wl_buffer is destroyed.
        #[cfg(target_os = "linux")]
        let fd = unsafe { libc::memfd_create(c"rumstsc-cursor".as_ptr(), libc::MFD_CLOEXEC) };
        #[cfg(target_os = "freebsd")]
        let fd = unsafe { libc::shm_open(libc::SHM_ANON, libc::O_RDWR | libc::O_CLOEXEC, 0o600) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let pixels = image.premultiplied(scale as u16);
        file.set_len(pixels.len() as u64)?;
        file.write_all(&pixels)?;
        let pool = self
            .shm
            .create_pool(file.as_fd(), pixels.len() as i32, &self.qh, ());
        let width = image.width as i32 * scale;
        let height = image.height as i32 * scale;
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        self.surface.set_buffer_scale(scale);
        pointer.set_cursor(
            serial,
            Some(&self.surface),
            image.hot_x as i32,
            image.hot_y as i32,
        );
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface
            .damage(0, 0, image.width as i32, image.height as i32);
        self.surface.commit();
        if let Some(previous) = self.custom_buffer.replace(buffer) {
            previous.destroy();
        }
        self.custom_key = Some((id, scale));
        Ok(())
    }
    pub fn set_icon(
        &mut self,
        wl_pointer: &WlPointer,
        serial_id: u32,
        mut cursor_icon_names: &[&str],
        scale: i32,
    ) {
        self.set_scaled_size(self.size * scale as u32);

        let Some(loaded_theme) = &mut self.loaded_theme else {
            log::warn!("Wayland: Unable to load cursor themes");
            return;
        };
        let mut theme = &mut loaded_theme.theme;

        let mut buffer: &CursorImageBuffer;
        'outer: {
            for cursor_icon_name in cursor_icon_names {
                if let Some(cursor) = theme.get_cursor(cursor_icon_name) {
                    buffer = &cursor[0];
                    break 'outer;
                }
            }

            if let Some(cursor) = theme.get_cursor(DEFAULT_CURSOR_ICON_NAME) {
                buffer = &cursor[0];
                log_cursor_icon_warning(anyhow!(
                    "wayland: Unable to get cursor icon {:?}. \
                    Using default cursor icon: '{}'",
                    cursor_icon_names,
                    DEFAULT_CURSOR_ICON_NAME
                ));
            } else {
                log_cursor_icon_warning(anyhow!(
                    "wayland: Unable to fallback on default cursor icon '{}' for theme '{}'",
                    DEFAULT_CURSOR_ICON_NAME,
                    loaded_theme.name.as_deref().unwrap_or("default")
                ));
                return;
            }
        }

        let (width, height) = buffer.dimensions();
        let (hot_x, hot_y) = buffer.hotspot();

        self.surface.set_buffer_scale(scale);

        wl_pointer.set_cursor(
            serial_id,
            Some(&self.surface),
            hot_x as i32 / scale,
            hot_y as i32 / scale,
        );

        self.surface.attach(Some(buffer), 0, 0);
        self.surface.damage(0, 0, width as i32, height as i32);
        self.surface.commit();
        self.custom_key = None;
        if let Some(previous) = self.custom_buffer.take() {
            previous.destroy();
        }
    }
}
