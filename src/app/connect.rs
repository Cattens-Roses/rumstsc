use super::*;
impl Client {
    pub(super) fn form(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.settings.theme.palette();
        let busy = self.connection.is_some();
        let mut card = div()
            .w_full()
            .flex_shrink_0()
            .max_w(px(480.))
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(p.panel)
            .border_1()
            .border_color(p.border)
            .rounded(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(desktop_mark(p.accent, p.soft, 40.))
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(p.accent)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("REMOTE DESKTOP"),
                    )
                    .child(
                        div()
                            .text_size(px(28.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Connect to a desktop"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(p.muted)
                            .child("Enter your computer details to get started."),
                    ),
            );
        if !busy {
            card = card
                .child(div().h(px(1.)).bg(p.border))
                .child(field("Computer", self.host.clone(), p.muted))
                .child(field("User name", self.user.clone(), p.muted))
                .child(field("Password", self.password.clone(), p.muted))
                .when(!self.settings.recent.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .gap_2()
                            .flex_wrap()
                            .child(div().py_1().text_xs().text_color(p.muted).child("Recent"))
                            .children(self.settings.recent.iter().take(3).enumerate().map(
                                |(i, h)| {
                                    let host = h.clone();
                                    div()
                                        .id(("recent", i))
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(p.muted)
                                        .bg(p.field)
                                        .hover(|s| s.bg(p.soft).text_color(p.accent))
                                        .cursor_pointer()
                                        .child(h.clone())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.host.update(cx, |f, cx| f.set(host.clone(), cx))
                                        }))
                                },
                            )),
                    )
                })
                .child(
                    div()
                        .id("options")
                        .flex()
                        .items_center()
                        .justify_between()
                        .py_2()
                        .border_b_1()
                        .border_color(p.border)
                        .hover(|s| s.text_color(p.text))
                        .text_sm()
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child(if self.advanced {
                            "Advanced options   -"
                        } else {
                            "Advanced options   +"
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
                            .p_4()
                            .bg(p.field)
                            .rounded(px(10.))
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
                                                .bg(if self.security_mode == mode {
                                                    p.soft
                                                } else {
                                                    p.panel
                                                })
                                                .text_color(if self.security_mode == mode {
                                                    p.accent
                                                } else {
                                                    p.muted
                                                })
                                                .hover(|s| s.bg(p.soft))
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
            card = card.child(
                div()
                    .p_3()
                    .rounded(px(8.))
                    .bg(p.field)
                    .border_1()
                    .border_color(p.error)
                    .text_sm()
                    .text_color(p.error)
                    .child(error.clone()),
            );
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
        card=card.child(div().id("connect").h(px(46.)).flex().items_center().justify_center().rounded(px(8.)).bg(if busy{p.soft}else{p.accent}).text_color(button_text).font_weight(FontWeight::SEMIBOLD).cursor_pointer().hover(|s| s.opacity(0.88)).child(if busy{"Cancel connection"}else{"Connect to desktop  →"}).on_click(cx.listener(|this,_,window,cx|{if this.connection.is_some(){this.disconnect(window, cx);}else{this.connect(window,cx);}})))
            .child(div().text_xs().text_color(p.muted).child("Passwords are never saved. Server identity is verified with system trust or your certificate pin. Legacy connections require a pin."));
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
}
