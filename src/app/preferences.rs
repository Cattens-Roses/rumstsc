use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsTab {
    Appearance,
    Display,
    Performance,
    Privacy,
}

impl Client {
    pub(super) fn open_settings(&mut self, open: bool, cx: &mut Context<Self>) {
        self.release_input();
        self.show_settings = open;
        if open && let Some(display) = &self.native {
            display.handle().bounds(rumstsc_display::Bounds::default());
        }
        cx.notify();
    }
    fn set_private_screen(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let result = self
            .native_parent
            .ok_or_else(|| "Cannot find this application window".to_string())
            .and_then(|parent| rumstsc_display::set_private_screen(parent, enabled));
        match result {
            Ok(()) => {
                self.private_screen_active = enabled;
                self.private_screen_error = None;
                self.settings.private_screen = enabled;
                self.persist();
            }
            Err(error) => {
                self.private_screen_active = false;
                self.private_screen_error = Some(error);
            }
        }
        cx.notify();
    }
    fn private_screen_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.settings.theme.palette();
        if !cfg!(windows) {
            return div().text_xs().text_color(p.muted)
                .child("Unavailable on Linux/X11/Wayland in this build. No capture protection is active.")
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().gap_2().children(
                [(false, "Off"), (true, "On")].into_iter().enumerate().map(
                    |(i, (enabled, label))| {
                        choice(
                            ("private-screen", i),
                            label,
                            self.private_screen_active == enabled,
                            p,
                        )
                        .flex_1()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_private_screen(enabled, cx)),
                        )
                    },
                ),
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted)
                    .child(if self.private_screen_active {
                        "Windows capture exclusion is active for this application window."
                    } else {
                        "Capture exclusion is not active."
                    }),
            )
            .when(self.private_screen_error.is_some(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(p.error)
                        .child(self.private_screen_error.clone().unwrap_or_default()),
                )
            })
            .into_any_element()
    }
    fn preferences_changed(&mut self, cx: &mut Context<Self>) {
        self.persist();
        cx.notify();
    }
    pub(super) fn presentation_interval(&self) -> Duration {
        if self.show_settings || (!self.window_active && self.settings.pause_background) {
            Duration::from_millis(200)
        } else if !self.active {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(self.settings.power_profile.interval_ms())
        }
    }
    pub(super) fn preferences(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.settings.theme.palette();
        let content = match self.settings_tab {
            SettingsTab::Appearance => section("Appearance", "Make the workspace comfortable for you.", p)
                .child(setting("Theme", "Choose the palette for the entire application.",
                    div().flex().gap_2().children([(Theme::Dark,"Dark"),(Theme::Light,"Light")].into_iter().enumerate().map(|(i,(theme,label))|
                        choice(("theme",i),label,self.settings.theme == theme,p).flex_1()
                            .on_click(cx.listener(move |this,_,_,cx| {
                                if this.settings.theme != theme { this.toggle_theme(cx); }
                            })))), p))
                .child(setting("Status bar", "Show connection, resolution and input details at the bottom.",
                    choice("show-status", if self.settings.show_status { "On" } else { "Off" },self.settings.show_status,p)
                        .on_click(cx.listener(|this,_,_,cx| { this.settings.show_status = !this.settings.show_status; this.preferences_changed(cx); })),p)),
            SettingsTab::Display => section("Remote display", "Resolution and renderer changes apply to your next connection.", p)
                .child(setting("Desktop resolution", "A smaller remote desktop requires fewer pixels to decode and draw.",
                    div().flex().flex_wrap().gap_2().children(RESOLUTIONS.iter().enumerate().map(|(i,(w,h))|
                        choice(("resolution",i),format!("{w} × {h}"), self.settings.resolution==i,p)
                            .on_click(cx.listener(move |this,_,_,cx| { this.settings.resolution=i; this.preferences_changed(cx); })))),p))
                .child(setting("Desktop renderer", "Native uses an embedded system window on Windows or X11. GPUI also supports Wayland.",
                    div().flex().gap_2().children([(false,"GPUI"),(true,"Native")].into_iter().enumerate().map(|(i,(native,label))|
                        choice(("renderer",i),label,self.settings.native_display==native,p).flex_1()
                            .on_click(cx.listener(move |this,_,_,cx| { this.settings.native_display=native; this.preferences_changed(cx); })))),p))
                .child(setting("Connect in full screen", "Enter full screen when the remote session becomes active.",
                    choice("start-fullscreen",if self.settings.start_fullscreen { "On" } else { "Off" },self.settings.start_fullscreen,p)
                        .on_click(cx.listener(|this,_,_,cx| { this.settings.start_fullscreen = !this.settings.start_fullscreen; this.preferences_changed(cx); })),p)),
            SettingsTab::Performance => section("Performance & battery", "Choose how often the desktop is presented. Changes apply immediately.", p)
                .child(setting("Power profile", "Battery saver limits presentation to 15 updates/s. It reduces UI wakeups and image uploads at the cost of smoothness.",
                    div().flex().flex_col().gap_2().children([
                        (PowerProfile::Smooth,"Smooth, up to 60 updates/s"),
                        (PowerProfile::Balanced,"Balanced, up to 30 updates/s"),
                        (PowerProfile::Battery,"Battery saver, up to 15 updates/s"),
                    ].into_iter().enumerate().map(|(i,(profile,label))|
                        choice(("power",i),label,self.settings.power_profile==profile,p)
                            .on_click(cx.listener(move |this,_,_,cx| { this.settings.power_profile=profile; this.preferences_changed(cx); })))),p))
                .child(setting("Pause presentation in the background", "Stop desktop uploads and painting while this window is unfocused. The session remains connected.",
                    choice("pause-background",if self.settings.pause_background { "On" } else { "Off" },self.settings.pause_background,p)
                        .on_click(cx.listener(|this,_,_,cx| { this.settings.pause_background = !this.settings.pause_background; this.preferences_changed(cx); })),p))
                .child(div().p_3().rounded_md().bg(p.field).text_xs().text_color(p.muted)
                    .child("Network traffic and protocol decoding continue so remote updates stay correct. For further savings, choose a lower resolution in Display.")),
            SettingsTab::Privacy => section("Privacy & history", "Passwords are never saved. Certificate trust still requires verification.", p)
                .child(setting("Private screen", "Windows 10 (2004+) / 11: exclude this local window from compatible screenshot and recording APIs. This is not DRM and does not guarantee blocking Veyon, administrator tools, cameras or recording on the remote server.", self.private_screen_control(cx), p))
                .child(setting("Remember recent computers", "Save up to five successful connection addresses on this device. Turning this off also clears existing history.",
                    choice("remember-recent",if self.settings.remember_recent { "On" } else { "Off" },self.settings.remember_recent,p)
                        .on_click(cx.listener(|this,_,_,cx| { this.settings.remember_recent = !this.settings.remember_recent;
                            if !this.settings.remember_recent { this.settings.recent.clear(); }
                            this.preferences_changed(cx); })),p))
                .child(setting("Connection history", "Clear saved addresses without changing other preferences.",
                    choice("clear-history",format!("Clear history ({})",self.settings.recent.len()),false,p)
                        .on_click(cx.listener(|this,_,_,cx| { this.settings.recent.clear(); this.preferences_changed(cx); })),p)),
        };
        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(540.))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(28.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Settings"),
                            )
                            .child(
                                div().text_sm().text_color(p.muted).child(
                                    "Your workspace, your preferences. Saved automatically.",
                                ),
                            ),
                    )
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            [
                                (SettingsTab::Appearance, "Appearance"),
                                (SettingsTab::Display, "Display"),
                                (SettingsTab::Performance, "Performance"),
                                (SettingsTab::Privacy, "Privacy"),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(i, (tab, label))| {
                                choice(("settings-tab", i), label, self.settings_tab == tab, p)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.settings_tab = tab;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    .child(content)
                    .when(self.error.is_some(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(p.error)
                                .child(self.error.clone().unwrap_or_default()),
                        )
                    }),
            )
            .into_any_element()
    }
}
