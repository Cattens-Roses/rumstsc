use super::*;
impl Render for Client {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_render {
            self.startup_trace.mark("first_view_render", "ui", 0);
            self.first_render = true;
        }
        if self.pending_fullscreen {
            self.pending_fullscreen = false;
            if !window.is_fullscreen() {
                window.toggle_fullscreen();
            }
        }
        let p = self.settings.theme.palette();
        let active = self.active;
        let immersive = window.is_fullscreen() && !self.show_settings && !self.fullscreen_controls;
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
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if !window.is_fullscreen() || this.show_settings || this.mouse != 0 {
                    return;
                }
                let size = window.viewport_size();
                let corner = e.position.x >= size.width - px(32.) && e.position.y <= px(32.);
                let desktop = e.position.y > px(64.) && e.position.y < size.height - px(36.);
                if corner && !this.fullscreen_controls {
                    this.fullscreen_controls = true;
                    cx.notify();
                } else if desktop && this.fullscreen_controls {
                    this.fullscreen_controls = false;
                    cx.notify();
                }
            }))
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
            .when(!immersive, |d| d.child(self.header(window, cx)))
            .when(self.clipboard_error.is_some(), |d| {
                d.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_xs()
                        .text_color(p.error)
                        .child(self.clipboard_error.clone().unwrap_or_default()),
                )
            })
            .when(self.private_screen_error.is_some(), |d| {
                d.child(
                    div()
                        .px_4()
                        .py_2()
                        .bg(p.panel)
                        .text_xs()
                        .text_color(p.error)
                        .child(format!(
                            "Private screen is not active: {}",
                            self.private_screen_error
                                .as_deref()
                                .unwrap_or("unavailable")
                        )),
                )
            })
            .child(if self.show_settings {
                self.preferences(cx)
            } else if active {
                self.remote(cx)
            } else {
                self.form(cx)
            })
            .when(self.settings.show_status && !immersive, |d| {
                d.child(self.status_bar())
            })
    }
}
impl Client {
    pub(super) fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.release_input();
        self.fullscreen_controls = false;
        window.toggle_fullscreen();
        cx.notify();
    }
    fn header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.settings.theme.palette();
        div()
            .h(px(64.))
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
                    .child(desktop_mark(p.accent, p.soft, 30.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("rumstsc")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.active, |d| {
                        d.child(
                            choice("disconnect", "Disconnect", false, p)
                                .text_color(p.error)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.disconnect(window, cx)),
                                ),
                        )
                    })
                    .child(
                        choice(
                            "fullscreen",
                            if window.is_fullscreen() {
                                "Windowed"
                            } else {
                                "Full screen"
                            },
                            false,
                            p,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_fullscreen(window, cx);
                        })),
                    )
                    .child(
                        choice(
                            "preferences",
                            if self.show_settings {
                                if self.active {
                                    "Back to session"
                                } else {
                                    "Back"
                                }
                            } else {
                                "Settings"
                            },
                            self.show_settings,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| {
                                this.open_settings(!this.show_settings, cx)
                            }),
                        ),
                    ),
            )
    }
    fn status_bar(&self) -> impl IntoElement {
        let p = self.settings.theme.palette();
        div()
            .h(px(36.))
            .flex_shrink_0()
            .px_5()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .border_t_1()
            .border_color(p.border)
            .text_xs()
            .text_color(p.muted)
            .child(div().overflow_hidden().child(if self.show_settings {
                "Preferences are saved automatically".into()
            } else if self.active {
                format!(
                    "{} × {} · {}",
                    self.desktop.0, self.desktop.1, self.graphics_mode
                )
            } else {
                self.status.clone()
            }))
            .child(if self.active && !self.show_settings {
                "Ctrl+Alt+Home shows controls"
            } else {
                match self.settings.power_profile {
                    PowerProfile::Smooth => "Smooth",
                    PowerProfile::Balanced => "Balanced",
                    PowerProfile::Battery => "Battery saver",
                }
            })
    }
}
