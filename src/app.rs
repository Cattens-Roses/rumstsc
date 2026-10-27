use crate::{input::TextField, keyboard::scancode, settings::Settings, theme::Theme};
use gpui::{prelude::*, *};
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
    time::Duration,
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
        #[cfg(not(any(windows, target_os = "linux")))]
        let native_parent = None;
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
            if !window.is_window_active() {
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
            _subscriptions: vec![blur, activation],
        }
    }
    fn persist(&mut self) {
        if let Err(e) = self.settings.save() {
            self.error = Some(format!("Could not save preferences: {e}"));
        }
    }
    fn clear_texture(&mut self, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
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
        };
        match Connection::start_interactive_traced(options, Settings::trace_path()) {
            Ok(connection) => {
                self.password.update(cx, |f, cx| f.set(String::new(), cx));
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
                Timer::after(Duration::from_millis(16)).await;
                if !matches!(this.update(cx, |this, cx| this.poll(cx)), Ok(true)) {
                    break;
                }
            }
        }));
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(c) = &self.connection else {
            return false;
        };
        let events: Vec<_> = c.events.try_iter().collect();
        let mut closed = false;
        let mut changed = false;
        for event in events {
            changed = true;
            match event {
                Event::CertificateReview(certificate) => {
                    self.status = "Review server certificate".into();
                    self.certificate_review = Some(certificate);
                }
                Event::Security(label) => self.session_security = label,
                Event::Graphics(label) => self.graphics_mode = label.into(),
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
                            Ok(display) => self.native = Some(display),
                            Err(error) => self.error = Some(error),
                        }
                    }
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
                changed = true;
            }
        }
        // Never block the UI while the worker decodes a bitmap. Coalesce updates
        // into the latest complete snapshot and retry next tick when it is busy.
        let snapshot = self.connection.as_ref().and_then(|c| c.take_snapshot());
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
        if let Some(connection) = &self.connection {
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
            c.send(input);
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
        self.mouse = 0;
        self.modifiers = Modifiers::default();
        self.capture = false;
    }
    fn disconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.release_input();
        self.poll_task = None;
        self.native = None;
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
        let mods = e.keystroke.modifiers;
        if mods.control && mods.alt && key == "home" {
            self.release_input();
            self.status = "Input released · click the desktop to resume".into();
            cx.notify();
            return;
        }
        if self.active {
            if self.capture {
                self.modifiers(mods);
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
    fn form(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.settings.theme.palette();
        let busy = self.connection.is_some();
        let mut card = div()
            .w_full()
            .flex_shrink_0()
            .max_w(px(440.))
            .flex()
            .flex_col()
            .gap_5()
            .p_6()
            .bg(p.panel)
            .border_1()
            .border_color(p.border)
            .rounded_lg()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Remote Desktop Connection"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(p.muted)
                            .child("A small window to another computer."),
                    ),
            );
        if !busy {
            card = card
                .child(field("Computer", self.host.clone(), p.muted))
                .child(field("User name", self.user.clone(), p.muted))
                .child(field("Password", self.password.clone(), p.muted))
                .when(!self.settings.recent.is_empty(), |d| {
                    d.child(
                        div().flex().gap_2().flex_wrap().children(
                            self.settings
                                .recent
                                .iter()
                                .take(3)
                                .enumerate()
                                .map(|(i, h)| {
                                    let host = h.clone();
                                    div()
                                        .id(("recent", i))
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(p.muted)
                                        .bg(p.field)
                                        .cursor_pointer()
                                        .child(h.clone())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.host.update(cx, |f, cx| f.set(host.clone(), cx))
                                        }))
                                }),
                        ),
                    )
                })
                .child(
                    div()
                        .id("options")
                        .text_sm()
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child(if self.advanced {
                            "− Hide options"
                        } else {
                            "+ Show options"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.advanced = !this.advanced;
                            cx.notify();
                        })),
                )
                .when(self.advanced, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(field("Domain", self.domain.clone(), p.muted))
                            .child(
                                div().flex().flex_wrap().gap_2().children(
                                    [
                                        (SecurityMode::Auto, "TLS / NLA"),
                                        (SecurityMode::Nla, "NLA only"),
                                        (SecurityMode::Tls, "TLS only"),
                                        (SecurityMode::Legacy, "Legacy"),
                                    ]
                                    .into_iter()
                                    .enumerate()
                                    .map(
                                        |(i, (mode, label))| {
                                            div()
                                                .id(("security", i))
                                                .px_2()
                                                .py_1()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(if self.security_mode == mode {
                                                    p.accent
                                                } else {
                                                    p.border
                                                })
                                                .cursor_pointer()
                                                .text_xs()
                                                .child(label)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.security_mode = mode;
                                                    cx.notify();
                                                }))
                                        },
                                    ),
                                ),
                            )
                            .child(field(
                                "Certificate SHA-256 pin (optional for TLS)",
                                self.certificate_pin.clone(),
                                p.muted,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("native-display")
                                            .px_2()
                                            .py_2()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(p.border)
                                            .text_sm()
                                            .cursor_pointer()
                                            .child(if self.settings.native_display {
                                                "Display: native framebuffer"
                                            } else {
                                                "Display: GPUI"
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.settings.native_display =
                                                    !this.settings.native_display;
                                                this.persist();
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        div().text_xs().text_color(p.muted).child("DESKTOP SIZE"),
                                    )
                                    .child(div().flex().gap_2().children(
                                        RESOLUTIONS.iter().enumerate().map(|(i, (w, h))| {
                                            div()
                                                .id(("resolution", i))
                                                .flex_1()
                                                .py_2()
                                                .text_center()
                                                .text_xs()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(if self.settings.resolution == i {
                                                    p.accent
                                                } else {
                                                    p.border
                                                })
                                                .cursor_pointer()
                                                .child(format!("{w} × {h}"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.settings.resolution = i;
                                                    this.persist();
                                                    cx.notify();
                                                }))
                                        }),
                                    )),
                            )
                            .child(
                                div()
                                    .id("insecure")
                                    .flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(p.muted)
                                    .cursor_pointer()
                                    .child(if self.insecure { "[✓]" } else { "[ ]" })
                                    .child("Allow plaintext RDP for an isolated test server")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.insecure = !this.insecure;
                                        cx.notify();
                                    })),
                            ),
                    )
                });
        }
        if let Some(error) = &self.error {
            card = card.child(div().text_sm().text_color(p.error).child(error.clone()));
        }
        if let Some(certificate) = &self.certificate_review {
            let fingerprint = certificate.fingerprint.clone();
            // Split the hash into short lines so the entire fingerprint is visible
            // on the compact connection card in either theme.
            let fingerprint_lines = fingerprint
                .as_bytes()
                .chunks(32)
                .map(|line| String::from_utf8_lossy(line).into_owned())
                .collect::<Vec<_>>();
            card = card.child(div().flex().flex_col().gap_3()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Server identity could not be verified"))
                .child(div().text_sm().child(format!("Computer: {}", certificate.host)))
                .child(div().text_xs().text_color(p.error).child(certificate.reason.clone()))
                .child(div().text_xs().child(format!("Subject: {}", certificate.subject)))
                .child(div().text_xs().child(format!("Issuer: {}", certificate.issuer)))
                .child(div().text_xs().text_color(p.muted).child("CERTIFICATE SHA-256"))
                .child(div().flex().flex_col().text_xs().children(fingerprint_lines.into_iter().map(|line| div().child(line))))
                .child(div().text_sm().child("Compare this fingerprint with the server administrator through a trusted channel. Continuing accepts this identity for this connection only, including any certificate name or validity errors. Credentials will be sent only after approval."))
                .child(div().id("trust-certificate-once").px_3().py_2().rounded_md()
                    .border_1().border_color(p.accent).text_color(p.accent).cursor_pointer()
                    .child("Trust once and connect")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(connection) = &this.connection {
                            connection.trust_certificate_once(&fingerprint);
                        }
                        this.certificate_review = None;
                        this.status = "Completing TLS verification…".into();
                        cx.notify();
                    }))));
        } else if busy {
            card = card.child(
                div()
                    .text_sm()
                    .text_color(p.accent)
                    .child(self.status.clone()),
            );
        }
        let button_text = if busy {
            p.text
        } else if self.settings.theme == Theme::Dark {
            rgb(0x102238).into()
        } else {
            rgb(0xffffff).into()
        };
        card=card.child(div().id("connect").h(px(42.)).flex().items_center().justify_center().rounded_md().bg(if busy{p.soft}else{p.accent}).text_color(button_text).font_weight(FontWeight::SEMIBOLD).cursor_pointer().child(if busy{"Cancel"}else{"Connect"}).on_click(cx.listener(|this,_,window,cx|{if this.connection.is_some(){this.disconnect(window, cx);}else{this.connect(window,cx);}})))
            .child(div().text_xs().text_color(p.muted).child("TLS verifies system trust or an explicit certificate pin. Passwords are never saved. Legacy encryption requires a pin."));
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .id("form-scroll")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .p_6()
            .child(card)
            .into_any_element()
    }
    fn remote(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(display) = &self.native {
            let display = display.handle();
            return div()
                .id("native-desktop")
                .flex_1()
                .min_h_0()
                .w_full()
                .relative()
                .bg(rgb(0x07090b))
                .child(
                    canvas(
                        move |bounds, window, _| {
                            let scale = window.scale_factor();
                            display.bounds(rumstsc_display::Bounds {
                                x: (f32::from(bounds.left()) * scale).round() as i32,
                                y: (f32::from(bounds.top()) * scale).round() as i32,
                                width: (f32::from(bounds.size.width) * scale).round().max(0.)
                                    as u32,
                                height: (f32::from(bounds.size.height) * scale).round().max(0.)
                                    as u32,
                            });
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .into_any_element();
        }
        let tiles: Vec<_> = self.tiles.values().cloned().collect();
        let trace = self.connection.as_ref().map(|c| c.trace.clone());
        let first_paint = self.first_paint.clone();
        let (width, height) = self.desktop;
        let entity = cx.entity();
        let has_image = !tiles.is_empty();
        let mut desktop = div()
            .id("desktop")
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(0x07090b))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    this.focus.focus(window);
                    this.capture = true;
                    this.mouse |= 0x1000;
                    this.pointer(e.position, 0x9000);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, _, _| {
                    this.mouse |= 0x2000;
                    this.pointer(e.position, 0xa000);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseDownEvent, _, _| {
                    this.mouse |= 0x4000;
                    this.pointer(e.position, 0xc000);
                }),
            )
            .on_mouse_move(
                cx.listener(|this, e: &MouseMoveEvent, _, _| this.pointer(e.position, 0x0800)),
            );
        for (button, flag) in [
            (MouseButton::Left, 0x1000),
            (MouseButton::Right, 0x2000),
            (MouseButton::Middle, 0x4000),
        ] {
            desktop = desktop
                .on_mouse_up(
                    button,
                    cx.listener(move |this, e: &MouseUpEvent, _, _| {
                        this.mouse &= !flag;
                        this.pointer(e.position, flag);
                    }),
                )
                .on_mouse_up_out(
                    button,
                    cx.listener(move |this, e: &MouseUpEvent, _, _| {
                        this.mouse &= !flag;
                        this.pointer(e.position, flag);
                    }),
                );
        }
        desktop = desktop
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, _| {
                let delta = e.delta.pixel_delta(px(20.)).y;
                let flags = if delta < px(0.) {
                    0x0200 | 0x0100 | 0x88
                } else {
                    0x0200 | 120
                };
                this.pointer(e.position, flags);
            }))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let scale = (bounds.size.width / px(width as f32))
                            .min(bounds.size.height / px(height as f32));
                        let sz = size(px(width as f32 * scale), px(height as f32 * scale));
                        let b = Bounds::new(
                            point(
                                bounds.left() + (bounds.size.width - sz.width) / 2.,
                                bounds.top() + (bounds.size.height - sz.height) / 2.,
                            ),
                            sz,
                        );
                        entity.update(cx, |this, _| {
                            this.bounds = Some(b);
                        });
                        b
                    },
                    move |_, bounds, window, _| {
                        let _span = trace
                            .as_ref()
                            .map(|t| t.span("tile_upload_submit", "ui", tiles.len() as u64));
                        let has_tiles = !tiles.is_empty();
                        let scale = bounds.size.width / px(width as f32);
                        for tile in tiles {
                            let tile_bounds = Bounds::new(
                                point(
                                    bounds.left() + px(tile.left as f32 * scale),
                                    bounds.top() + px(tile.top as f32 * scale),
                                ),
                                size(
                                    px(tile.width as f32 * scale),
                                    px(tile.height as f32 * scale),
                                ),
                            );
                            let _ = window.paint_image(
                                tile_bounds,
                                Corners::default(),
                                tile.image,
                                0,
                                false,
                            );
                        }
                        if has_tiles
                            && !first_paint.swap(true, Ordering::Relaxed)
                            && let Some(trace) = &trace
                        {
                            trace.mark("first_frame_submitted", "ui", 0);
                        }
                    },
                )
                .size_full(),
            );
        if !has_image {
            desktop = desktop.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(0x9aa5af))
                    .child("Waiting for the first desktop update…"),
            );
        }
        desktop.into_any_element()
    }
}
fn field(label: &'static str, input: Entity<TextField>, muted: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_sm().text_color(muted).child(label))
        .child(input)
}
impl Render for Client {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_render {
            self.startup_trace.mark("first_view_render", "ui", 0);
            self.first_render = true;
        }
        let p = self.settings.theme.palette();
        let active = self.active;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.bg)
            .text_color(p.text)
            .font_family(if cfg!(target_os = "linux") {
                "sans-serif"
            } else {
                "Segoe UI"
            })
            .text_sm()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_key_up(cx.listener(|this, e: &KeyUpEvent, _, cx| {
                if let Some((code, extended)) = scancode(&e.keystroke.key)
                    && this.held.remove(&(code, extended))
                {
                    this.send(Input::Scancode {
                        code,
                        extended,
                        released: true,
                    });
                }
                if this.capture {
                    cx.stop_propagation();
                }
            }))
            .on_modifiers_changed(
                cx.listener(|this, e: &ModifiersChangedEvent, _, _| this.modifiers(e.modifiers)),
            )
            .child(
                div()
                    .h(px(58.))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().text_color(p.accent).text_size(px(24.)).child("▣"))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("rumstsc")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .when(active, |d| {
                                d.child(
                                    div()
                                        .id("disconnect")
                                        .cursor_pointer()
                                        .text_color(p.error)
                                        .child("Disconnect")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.disconnect(window, cx)
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .id("theme")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(p.border)
                                    .cursor_pointer()
                                    .child(if self.settings.theme == Theme::Dark {
                                        "Light"
                                    } else {
                                        "Dark"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                            ),
                    ),
            )
            .child(if active {
                self.remote(cx)
            } else {
                self.form(cx)
            })
            .child(
                div()
                    .h(px(36.))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(p.border)
                    .text_xs()
                    .text_color(p.muted)
                    .child(if active {
                        format!(
                            "{} × {} · {} · {} · {}",
                            self.desktop.0,
                            self.desktop.1,
                            if self.native.is_some() {
                                "Native"
                            } else {
                                "GPUI"
                            },
                            self.graphics_mode,
                            if self.capture {
                                "Input captured"
                            } else {
                                "Input released"
                            }
                        )
                    } else {
                        self.status.clone()
                    })
                    .child(if active {
                        "Ctrl+Alt+Home releases input"
                    } else {
                        "Rust + GPUI"
                    }),
            )
    }
}
