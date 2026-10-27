use super::*;
impl Client {
    pub(super) fn remote(&self, cx: &mut Context<Self>) -> AnyElement {
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
                            display.upscale(window.is_fullscreen());
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
        let pointer_image = self.pointer_image.clone();
        let remote_pointer = self.remote_pointer.clone();
        let mut desktop = div()
            .id("desktop")
            .on_hover(cx.listener(|_, _, _, cx| cx.notify()))
            .cursor(
                if matches!(self.remote_pointer, rumstsc_rdp::pointer::Pointer::Default) {
                    CursorStyle::Arrow
                } else {
                    CursorStyle::None
                },
            )
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
                    this.fullscreen_controls = false;
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
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                this.pointer(e.position, 0x0800);
                if this.pointer_image.is_some() {
                    cx.notify();
                }
            }));
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
                        if let (rumstsc_rdp::pointer::Pointer::Image(pointer), Some(image)) =
                            (&remote_pointer, &pointer_image)
                        {
                            let position = window.mouse_position();
                            if bounds.contains(&position) {
                                let cursor_bounds = Bounds::new(
                                    point(
                                        position.x - px(pointer.hot_x as f32),
                                        position.y - px(pointer.hot_y as f32),
                                    ),
                                    size(px(pointer.width as f32), px(pointer.height as f32)),
                                );
                                let _ = window.paint_image(
                                    cursor_bounds,
                                    Corners::default(),
                                    image.clone(),
                                    0,
                                    false,
                                );
                            }
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
