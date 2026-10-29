mod chrome;
mod connect;
mod desktop;
mod preferences;
use crate::{
    input::TextField,
    keyboard::scancode,
    settings::{PowerProfile, Settings},
    theme::Theme,
    ui::*,
};
use gpui::{prelude::*, *};
use preferences::SettingsTab;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rumstsc_rdp::{
    negotiation::{Endpoint, SecurityMode},
    session::{Connection, Event, Input, Options},
    transport::CertificateReview,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const RESOLUTIONS: [(u16, u16); 3] = [(1280, 720), (1600, 900), (1920, 1080)];
#[derive(Clone)]
struct DesktopTile {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    image: Arc<RenderImage>,
}
pub struct Client {
    startup_trace: Arc<rumstsc_rdp::trace::Trace>,
    native_parent: Option<rumstsc_display::Parent>,
    native: Option<rumstsc_display::Display>,
    first_render: bool,
    settings: Settings,
    private_screen_active: bool,
    private_screen_error: Option<String>,
    clipboard_live: bool,
    clipboard_seen: Option<Option<String>>,
    clipboard_poll: Instant,
    clipboard_error: Option<String>,
    pub host: Entity<TextField>,
    user: Entity<TextField>,
    domain: Entity<TextField>,
    password: Entity<TextField>,
    certificate_pin: Entity<TextField>,
    security_mode: SecurityMode,
    session_security: String,
    graphics_mode: String,
    focus: FocusHandle,
    advanced: bool,
    show_settings: bool,
    settings_tab: SettingsTab,
    window_active: bool,
    pending_fullscreen: bool,
    fullscreen_controls: bool,
    insecure: bool,
    connection: Option<Connection>,
    poll_task: Option<Task<()>>,
    active: bool,
    capture: bool,
    status: String,
    error: Option<String>,
    certificate_review: Option<CertificateReview>,
    tiles: BTreeMap<usize, DesktopTile>,
    frame_generation: u64,
    first_paint: Arc<AtomicBool>,
    revision: u64,
    desktop: (u16, u16),
    bounds: Option<Bounds<Pixels>>,
    held: BTreeSet<(u16, bool)>,
    modifiers: Modifiers,
    mouse: u16,
    remote_pointer: rumstsc_rdp::pointer::Pointer,
    pointer_image: Option<Arc<RenderImage>>,
    pointer_dirty: bool,
    motion: desktop::Motion,
    framebuffer: Entity<desktop::Framebuffer>,
    _subscriptions: Vec<Subscription>,
}
impl Client {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        startup_trace: Arc<rumstsc_rdp::trace::Trace>,
    ) -> Self {
        let _span = startup_trace.span("client_ui_init", "ui", 0);
        let mut settings = Settings::load();
        if let Ok(renderer) = std::env::var("RUMSTSC_RENDERER") {
            settings.native_display = renderer == "native";
        }
        #[cfg(windows)]
        let native_parent = window
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(h) => Some(rumstsc_display::Parent::Windows(h.hwnd.get())),
                _ => None,
            });
        #[cfg(target_os = "linux")]
        let native_parent = std::env::var_os("DISPLAY")
            .map(|_| rumstsc_display::Parent::X11Process(std::process::id()));
        #[cfg(target_os = "linux")]
        if native_parent.is_some()
            && let Err(error) = rumstsc_display::set_application_icon(
                std::process::id(),
                include_bytes!("../assets/rumstsc.argb"),
            )
        {
            eprintln!("Unable to set X11 application icon: {error}");
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let native_parent = None;
        let mut private_screen_active = false;
        let mut private_screen_error = None;
        if settings.private_screen {
            match native_parent
                .ok_or_else(|| "Cannot find this application window".to_string())
                .and_then(|parent| rumstsc_display::set_private_screen(parent, true))
            {
                Ok(()) => private_screen_active = true,
                Err(error) => private_screen_error = Some(error),
            }
        }
        let theme = settings.theme;
        let host = cx.new(|cx| {
            TextField::new(
                settings.recent.first().cloned().unwrap_or_default(),
                "Hostname or IP address",
                theme,
                cx,
            )
        });
        let user = cx.new(|cx| TextField::new(String::new(), "Optional", theme, cx));
        let domain = cx.new(|cx| TextField::new(String::new(), "Optional", theme, cx));
        let password = cx.new(|cx| {
            let mut field = TextField::new(String::new(), "Required for NLA", theme, cx);
            field.secret = true;
            field
        });
        let certificate_pin = cx.new(|cx| {
            TextField::new(
                String::new(),
                "System trust, or SHA-256 fingerprint",
                theme,
                cx,
            )
        });
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |this, _, cx| {
            this.release_input();
            cx.notify();
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if !this.window_active {
                this.release_input();
                cx.notify();
            }
        });
        drop(_span);
        Self {
            startup_trace,
            native_parent,
            native: None,
            first_render: false,
            settings,
            private_screen_active,
            private_screen_error,
            clipboard_live: false,
            clipboard_seen: None,
            clipboard_poll: Instant::now(),
            clipboard_error: None,
            host,
            user,
            domain,
            password,
            certificate_pin,
            security_mode: SecurityMode::Auto,
            graphics_mode: "Bitmap".into(),
            session_security: String::new(),
            focus,
            advanced: false,
            show_settings: false,
            settings_tab: SettingsTab::Appearance,
            window_active: window.is_window_active(),
            pending_fullscreen: false,
            fullscreen_controls: false,
            insecure: false,
            connection: None,
            poll_task: None,
            active: false,
            capture: false,
            status: "Ready to connect".into(),
            error: None,
            certificate_review: None,
            tiles: BTreeMap::new(),
            frame_generation: 0,
            first_paint: Arc::new(AtomicBool::new(false)),
            revision: 0,
            desktop: (1280, 720),
            bounds: None,
            held: BTreeSet::new(),
            modifiers: Modifiers::default(),
            mouse: 0,
            remote_pointer: Default::default(),
            pointer_image: None,
            pointer_dirty: false,
            motion: desktop::Motion::default(),
            framebuffer: cx.new(|_| desktop::Framebuffer::default()),
            _subscriptions: vec![blur, activation],
        }
    }
    fn persist(&mut self) {
        if let Err(e) = self.settings.save() {
            self.error = Some(format!("Could not save preferences: {e}"));
        }
    }
    fn clear_texture(&mut self, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.framebuffer.update(cx, |frame, cx| {
            frame.key = None;
            frame.clear();
            cx.notify();
        });
        for (_, tile) in std::mem::take(&mut self.tiles) {
            // paint_image caches by image ID. Dropping the Arc alone does not
            // evict its GPU atlas entry, so retired desktop frames need eviction.
            cx.drop_image(tile.image, window.as_deref_mut());
        }
    }
    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.settings.theme.toggle();
        let theme = self.settings.theme;
        for field in [
            &self.host,
            &self.user,
            &self.domain,
            &self.password,
            &self.certificate_pin,
        ] {
            field.update(cx, |f, cx| {
                f.theme = theme;
                cx.notify();
            });
        }
        self.persist();
        cx.notify();
    }
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.is_some() {
            return;
        }
        let host = self.host.read(cx).value.trim().to_string();
        let endpoint = match Endpoint::parse(&host) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        let (width, height) = RESOLUTIONS[self.settings.resolution];
        let certificate_pin =
            match rumstsc_rdp::crypto::parse_pin(&self.certificate_pin.read(cx).value) {
                Ok(p) => p,
                Err(e) => {
                    self.error = Some(e.to_string());
                    cx.notify();
                    return;
                }
            };
        let options = Options {
            endpoint,
            username: self.user.read(cx).value.clone(),
            domain: self.domain.read(cx).value.clone(),
            password: self.password.read(cx).value.clone().into(),
            security_mode: self.security_mode,
            certificate_pin,
            width,
            height,
            allow_insecure: self.insecure,
            clipboard: self.settings.clipboard,
        };
        self.clipboard_live = self.settings.clipboard;
        self.clipboard_seen = None;
        self.clipboard_error = None;
        match Connection::start_interactive_traced(options, Settings::trace_path()) {
            Ok(connection) => {
                self.password.update(cx, |f, cx| f.set(String::new(), cx));
                self.remote_pointer = Default::default();
                self.pointer_image = None;
                self.connection = Some(connection);
                self.error = None;
                self.certificate_review = None;
                self.status = "Connecting…".into();
                self.active = false;
                self.capture = false;
                self.revision = 0;
                self.first_paint = Arc::new(AtomicBool::new(false));
                self.clear_texture(Some(window), cx);
                self.desktop = (width, height);
                self.graphics_mode = "Bitmap".into();
                self.focus.focus(window);
            }
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        }
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |this, _| this.presentation_interval()) else {
                    break;
                };
                Timer::after(interval).await;
                if !matches!(this.update(cx, |this, cx| this.poll(cx)), Ok(true)) {
                    break;
                }
            }
        }));
        cx.notify();
    }
    fn sync_clipboard(&mut self, request: Option<u64>, force: bool, cx: &mut Context<Self>) {
        if !self.clipboard_live {
            return;
        }
        let text = cx.read_from_clipboard().and_then(|item| item.text());
        if (force || self.clipboard_seen.as_ref() != Some(&text))
            && let Some(connection) = &self.connection
        {
            self.clipboard_seen = Some(text.clone());
            match connection.set_clipboard(text, request) {
                Ok(()) => self.clipboard_error = None,
                Err(error) => self.clipboard_error = Some(error.to_string()),
            }
            cx.notify();
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(c) = &self.connection else {
            return false;
        };
        self.motion.flush(|input| c.send(input));
        let events: Vec<_> = c.events.try_iter().collect();
        let mut closed = false;
        let mut changed = self.pointer_dirty;
        self.pointer_dirty = false;
        for event in events {
            if let Event::Pointer(pointer) = &event
                && self.remote_pointer.same_shape(pointer)
            {
                continue;
            }
            changed = true;
            match event {
                Event::ClipboardReady => self.sync_clipboard(None, true, cx),
                Event::ClipboardSync(request) => self.sync_clipboard(Some(request), true, cx),
                Event::ClipboardText(text) => {
                    if self.clipboard_live {
                        let text = if cfg!(target_os = "linux") {
                            text.replace("\r\n", "\n")
                        } else {
                            text
                        };
                        self.clipboard_seen = Some(Some(text.clone()));
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
                Event::ClipboardWarning(error) => self.clipboard_error = Some(error),
                Event::CertificateReview(certificate) => {
                    self.status = "Review server certificate".into();
                    self.certificate_review = Some(certificate);
                }
                Event::Security(label) => self.session_security = label,
                Event::Graphics(label) => self.graphics_mode = label.into(),
                Event::Pointer(pointer) => {
                    if let Some(image) = self.pointer_image.take() {
                        cx.drop_image(image, None);
                    }
                    self.pointer_image = match &pointer {
                        rumstsc_rdp::pointer::Pointer::Image(image) => image::RgbaImage::from_raw(
                            image.width.into(),
                            image.height.into(),
                            image.bgra.clone(),
                        )
                        .map(|pixels| Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]))),
                        _ => None,
                    };
                    if let Some(display) = &self.native {
                        display.pointer(pointer.clone());
                    }
                    self.remote_pointer = pointer;
                }
                Event::Status(s) => {
                    self.status = s;
                    if self.active {
                        self.active = false;
                        self.capture = false;
                    }
                }
                Event::Active => {
                    if self.settings.native_display
                        && let Some(parent) = self.native_parent
                    {
                        match rumstsc_display::Display::start(
                            parent,
                            self.connection.as_ref().unwrap().input_sender(),
                            self.connection.as_ref().unwrap().trace.clone(),
                        ) {
                            Ok(display) => {
                                display.pointer(self.remote_pointer.clone());
                                self.native = Some(display);
                            }
                            Err(error) => self.error = Some(error),
                        }
                    }
                    self.pending_fullscreen = self.settings.start_fullscreen;
                    self.active = true;
                    self.capture = true;
                    self.status = format!("Connected · {}", self.session_security);
                    let host = self.host.read(cx).value.trim().into();
                    self.settings.remember(host);
                    self.persist();
                }
                Event::Error(s) => {
                    self.error = Some(s);
                    self.status = "Connection failed".into();
                    closed = true;
                }
                Event::Closed => {
                    closed = true;
                    if self.error.is_none() {
                        self.status = "Remote connection closed".into();
                    }
                }
            }
        }
        if closed {
            self.release_input();
            self.native = None;
            self.connection = None;
            self.certificate_review = None;
            self.active = false;
            self.capture = false;
            self.clear_texture(None, cx);
            cx.notify();
            return false;
        }
        if self.active
            && self.clipboard_live
            && self.clipboard_poll.elapsed() >= Duration::from_millis(250)
        {
            self.clipboard_poll = Instant::now();
            self.sync_clipboard(None, false, cx);
        }
        if let Some(error) = self.native.as_ref().and_then(|n| n.error()) {
            self.native = None;
            self.clear_texture(None, cx);
            self.error = Some(format!("Native display failed; using GPUI: {error}"));
            changed = true;
        }
        if let Some(display) = &self.native {
            let captured = display.captured();
            if self.capture != captured {
                self.capture = captured;
                self.fullscreen_controls = !captured;
                changed = true;
            }
            if let Some(show) = display.take_controls_request()
                && self.fullscreen_controls != show
            {
                self.fullscreen_controls = show;
                changed = true;
            }
        }
        // Never block the UI while the worker decodes a bitmap. Coalesce updates
        // into the latest complete snapshot and retry next tick when it is busy.
        let presenting =
            !self.show_settings && (self.window_active || !self.settings.pause_background);
        let snapshot = if presenting {
            self.connection.as_ref().and_then(|c| c.take_snapshot())
        } else {
            None
        };
        if let Some(snapshot) = snapshot {
            if let Some(display) = &self.native {
                self.frame_generation = snapshot.generation;
                self.revision = snapshot.revision;
                if self.desktop != (snapshot.width, snapshot.height) {
                    changed = true;
                }
                self.desktop = (snapshot.width, snapshot.height);
                display.submit(snapshot);
            } else {
                let trace = self.connection.as_ref().unwrap().trace.clone();
                let _span = trace.span("tile_images_prepare", "ui", snapshot.tiles.len() as u64);
                if self.frame_generation != snapshot.generation {
                    self.clear_texture(None, cx);
                }
                self.frame_generation = snapshot.generation;
                self.revision = snapshot.revision;
                self.desktop = (snapshot.width, snapshot.height);
                for tile in snapshot.tiles {
                    if let Some(buffer) =
                        image::RgbaImage::from_raw(tile.width.into(), tile.height.into(), tile.bgra)
                    {
                        // GPUI expects BGRA bytes, despite the container type's name.
                        let image = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
                        if let Some(retired) = self.tiles.insert(
                            tile.index,
                            DesktopTile {
                                left: tile.left,
                                top: tile.top,
                                width: tile.width,
                                height: tile.height,
                                image,
                            },
                        ) {
                            cx.drop_image(retired.image, None);
                        }
                    }
                }
                changed = true;
            }
        }
        if presenting && let Some(connection) = &self.connection {
            connection.request_snapshot(self.frame_generation, self.revision);
        }
        if changed {
            cx.notify();
        }
        true
    }
    fn send(&self, input: Input) {
        if self.active
            && let Some(c) = &self.connection
        {
            self.motion.send(input, |input| c.send(input));
        }
    }
    fn release_input(&mut self) {
        for (code, extended) in std::mem::take(&mut self.held) {
            self.send(Input::Scancode {
                code,
                extended,
                released: true,
            });
        }
        for flag in [0x1000, 0x2000, 0x4000] {
            if self.mouse & flag != 0 {
                self.send(Input::Mouse {
                    x: 0,
                    y: 0,
                    flags: flag,
                });
            }
        }
        self.motion = desktop::Motion::default();
        self.mouse = 0;
        self.modifiers = Modifiers::default();
        self.capture = false;
    }
    fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.release_input();
        self.poll_task = None;
        self.native = None;
        self.remote_pointer = Default::default();
        self.pointer_image = None;
        self.connection = None;
        self.certificate_review = None;
        self.active = false;
        self.clear_texture(Some(window), cx);
        self.bounds = None;
        self.status = "Disconnected".into();
        cx.notify();
    }
    fn modifiers(&mut self, new: Modifiers) {
        if !self.capture {
            return;
        }
        for (before, after, code, extended) in [
            (self.modifiers.control, new.control, 0x1d, false),
            (self.modifiers.alt, new.alt, 0x38, false),
            (self.modifiers.shift, new.shift, 0x2a, false),
            (self.modifiers.platform, new.platform, 0x5b, true),
        ] {
            if before != after {
                self.send(Input::Scancode {
                    code,
                    extended,
                    released: !after,
                });
                if after {
                    self.held.insert((code, extended));
                } else {
                    self.held.remove(&(code, extended));
                }
            }
        }
        self.modifiers = new;
    }
    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = e.keystroke.key.as_str();
        let mut mods = e.keystroke.modifiers;
        mods.shift |= crate::keyboard::requires_shift(key);
        if self.show_settings {
            if key == "escape" {
                self.open_settings(false, cx);
            }
            return;
        }
        if (key == "f11" || (key == "escape" && window.is_fullscreen())) && !self.capture {
            self.toggle_fullscreen(window, cx);
            return;
        }
        if mods.control && mods.alt && key == "home" {
            self.release_input();
            self.fullscreen_controls = true;
            self.status = "Input released · click the desktop to resume".into();
            cx.notify();
            return;
        }
        if self.active {
            if self.capture {
                self.modifiers(mods);
                if mods.control && !mods.alt && key.eq_ignore_ascii_case("v") {
                    self.send(Input::Paste);
                    cx.stop_propagation();
                    return;
                }
                if let Some((code, extended)) = scancode(key) {
                    self.held.insert((code, extended));
                    self.send(Input::Scancode {
                        code,
                        extended,
                        released: false,
                    });
                }
                cx.stop_propagation();
            }
        } else if self.connection.is_none() {
            if key == "enter" {
                self.connect(window, cx);
            }
            if key == "tab" {
                let fields = if self.advanced {
                    vec![
                        &self.host,
                        &self.user,
                        &self.password,
                        &self.domain,
                        &self.certificate_pin,
                    ]
                } else {
                    vec![&self.host, &self.user, &self.password]
                };
                let current = fields
                    .iter()
                    .position(|f| f.read(cx).focus.is_focused(window));
                let i = match current {
                    Some(i) => {
                        if mods.shift {
                            (i + fields.len() - 1) % fields.len()
                        } else {
                            (i + 1) % fields.len()
                        }
                    }
                    None => 0,
                };
                fields[i].read(cx).focus.focus(window);
                cx.notify();
                cx.stop_propagation();
            }
        }
    }
    fn pointer(&mut self, p: Point<Pixels>, flags: u16) {
        let Some(b) = self.bounds else {
            return;
        };
        if !self.capture || b.size.width <= px(0.) || b.size.height <= px(0.) {
            return;
        }
        let x = ((p.x - b.left()) / b.size.width * self.desktop.0 as f32)
            .clamp(0., (self.desktop.0 - 1) as f32) as u16;
        let y = ((p.y - b.top()) / b.size.height * self.desktop.1 as f32)
            .clamp(0., (self.desktop.1 - 1) as f32) as u16;
        self.send(Input::Mouse { x, y, flags });
    }
}
