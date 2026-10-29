use super::*;

#[derive(Default)]
pub(super) struct Framebuffer {
    pub(super) key: Option<(u64, u64, usize)>,
    tiles: Vec<DesktopTile>,
    desktop: (u16, u16),
    trace: Option<Arc<rumstsc_rdp::trace::Trace>>,
    first_paint: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    paints: Arc<std::sync::atomic::AtomicUsize>,
}
fn fit(bounds: Bounds<Pixels>, width: u16, height: u16) -> Bounds<Pixels> {
    let (width, height) = (width.max(1), height.max(1));
    let scale = (bounds.size.width / px(width as f32)).min(bounds.size.height / px(height as f32));
    let sz = size(px(width as f32 * scale), px(height as f32 * scale));
    Bounds::new(
        point(
            bounds.left() + (bounds.size.width - sz.width) / 2.,
            bounds.top() + (bounds.size.height - sz.height) / 2.,
        ),
        sz,
    )
}
impl Framebuffer {
    fn cached(entity: Entity<Self>) -> AnyView {
        AnyView::from(entity).cached(StyleRefinement {
            position: Some(Position::Absolute),
            size: SizeRefinement {
                width: Some(relative(1.).into()),
                height: Some(relative(1.).into()),
            },
            ..Default::default()
        })
    }
    pub(super) fn clear(&mut self) {
        self.tiles.clear();
    }
}
impl Render for Framebuffer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let frame_tiles = self.tiles.clone();
        let self_trace = self.trace.clone();
        let frame_first_paint = self.first_paint.clone();
        let (width, height) = self.desktop;
        #[cfg(test)]
        let paints = self.paints.clone();
        canvas(
            move |bounds, _, _| fit(bounds, width, height),
            move |_, bounds, window, _| {
                #[cfg(test)]
                paints.fetch_add(1, Ordering::Relaxed);
                let _span = self_trace
                    .as_ref()
                    .map(|t| t.span("tile_upload_submit", "ui", frame_tiles.len() as u64));
                let has_frame_tiles = !frame_tiles.is_empty();
                let scale = bounds.size.width / px(width as f32);
                for tile in frame_tiles {
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
                    let _ =
                        window.paint_image(tile_bounds, Corners::default(), tile.image, 0, false);
                }
                if has_frame_tiles
                    && let Some(first_paint) = &frame_first_paint
                    && !first_paint.swap(true, Ordering::Relaxed)
                    && let Some(self_trace) = &self_trace
                {
                    self_trace.mark("first_frame_submitted", "ui", 0);
                }
            },
        )
        .size_full()
    }
}

impl Client {
    // Invalidate the cached child while processing updates, before GPUI starts
    // drawing. Notifying it from the parent's render is too late: that frame
    // may reuse atlas tiles already evicted by the snapshot update.
    pub(super) fn sync_framebuffer(&self, cx: &mut Context<Self>) {
        let key = (self.frame_generation, self.revision, self.tiles.len());
        self.framebuffer.update(cx, |frame, cx| {
            if frame.key != Some(key) {
                frame.key = Some(key);
                frame.tiles = self.tiles.values().cloned().collect();
                frame.desktop = self.desktop;
                frame.trace = self.connection.as_ref().map(|c| c.trace.clone());
                frame.first_paint = Some(self.first_paint.clone());
                cx.notify();
            }
        });
    }
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
        let (width, height) = self.desktop;
        let entity = cx.entity();
        let has_image = !self.tiles.is_empty();
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
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, _| {
                this.pointer(e.position, 0x0800);
                if this.pointer_image.is_some() {
                    this.pointer_dirty = true;
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
            .child(Framebuffer::cached(self.framebuffer.clone()))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let b = fit(bounds, width, height);
                        entity.update(cx, |this, _| {
                            this.bounds = Some(b);
                        });
                        b
                    },
                    move |_, bounds, window, _| {
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

/// Keep the latest position between presentation ticks; discrete input first
/// flushes it so dragging, clicks and keyboard shortcuts retain wire order.
#[derive(Default)]
pub(super) struct Motion {
    pending: std::cell::Cell<Option<Input>>,
    last: std::cell::Cell<Option<(u16, u16)>>,
}
impl Motion {
    pub(super) fn send(&self, input: Input, mut emit: impl FnMut(Input)) {
        if let Input::Mouse {
            x,
            y,
            flags: 0x0800,
        } = input
        {
            if self.last.replace(Some((x, y))) != Some((x, y)) {
                self.pending.set(Some(input));
            }
        } else {
            self.flush(&mut emit);
            if let Input::Mouse { x, y, .. } = input {
                self.last.set(Some((x, y)));
            }
            emit(input);
        }
    }
    pub(super) fn flush(&self, mut emit: impl FnMut(Input)) {
        if let Some(input) = self.pending.take() {
            emit(input);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{Input, Motion};
    #[test]
    fn high_rate_motion_keeps_latest_and_skips_same_remote_pixel() {
        let motion = Motion::default();
        let mut sent = Vec::new();
        for x in 0..1000 {
            motion.send(
                Input::Mouse {
                    x,
                    y: 8,
                    flags: 0x0800,
                },
                |i| sent.push(i),
            );
        }
        assert!(sent.is_empty());
        motion.flush(|i| sent.push(i));
        assert!(matches!(
            sent.as_slice(),
            [Input::Mouse {
                x: 999,
                y: 8,
                flags: 0x0800
            }]
        ));
        for _ in 0..1000 {
            motion.send(
                Input::Mouse {
                    x: 999,
                    y: 8,
                    flags: 0x0800,
                },
                |i| sent.push(i),
            );
            motion.flush(|i| sent.push(i));
        }
        assert_eq!(sent.len(), 1);
    }
    #[test]
    fn click_position_updates_motion_deduplication() {
        let motion = Motion::default();
        let mut sent = Vec::new();
        motion.send(
            Input::Mouse {
                x: 1,
                y: 2,
                flags: 0x0800,
            },
            |i| sent.push(i),
        );
        motion.send(
            Input::Mouse {
                x: 3,
                y: 4,
                flags: 0x9000,
            },
            |i| sent.push(i),
        );
        motion.send(
            Input::Mouse {
                x: 1,
                y: 2,
                flags: 0x0800,
            },
            |i| sent.push(i),
        );
        motion.flush(|i| sent.push(i));
        assert!(matches!(
            sent.as_slice(),
            [
                Input::Mouse { x: 1, .. },
                Input::Mouse { x: 3, .. },
                Input::Mouse {
                    x: 1,
                    flags: 0x0800,
                    ..
                }
            ]
        ));
    }
    #[test]
    fn motion_is_flushed_before_click_drag_release_and_keyboard() {
        let motion = Motion::default();
        let mut sent = Vec::new();
        for input in [
            Input::Mouse {
                x: 1,
                y: 2,
                flags: 0x0800,
            },
            Input::Mouse {
                x: 1,
                y: 2,
                flags: 0x9000,
            },
            Input::Mouse {
                x: 3,
                y: 4,
                flags: 0x0800,
            },
            Input::Mouse {
                x: 5,
                y: 6,
                flags: 0x0800,
            },
            Input::Mouse {
                x: 5,
                y: 6,
                flags: 0x1000,
            },
            Input::Mouse {
                x: 7,
                y: 8,
                flags: 0x0800,
            },
            Input::Paste,
        ] {
            motion.send(input, |i| sent.push(i));
        }
        assert!(matches!(
            sent.as_slice(),
            [
                Input::Mouse {
                    x: 1,
                    flags: 0x0800,
                    ..
                },
                Input::Mouse { flags: 0x9000, .. },
                Input::Mouse {
                    x: 5,
                    flags: 0x0800,
                    ..
                },
                Input::Mouse { flags: 0x1000, .. },
                Input::Mouse {
                    x: 7,
                    flags: 0x0800,
                    ..
                },
                Input::Paste
            ]
        ));
        motion.send(
            Input::Mouse {
                x: 9,
                y: 8,
                flags: 0x0800,
            },
            |i| sent.push(i),
        );
        motion.send(
            Input::Scancode {
                code: 0x1d,
                extended: false,
                released: false,
            },
            |i| sent.push(i),
        );
        assert!(matches!(
            &sent[6..],
            [
                Input::Mouse { x: 9, .. },
                Input::Scancode { code: 0x1d, .. }
            ]
        ));
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::{DesktopTile, Framebuffer};
    use gpui::{prelude::*, *};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Harness {
        frame: Entity<Framebuffer>,
        width: f32,
        renders: Arc<AtomicUsize>,
    }
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.fetch_add(1, Ordering::Relaxed);
            div()
                .relative()
                .w(px(self.width))
                .h(px(180.))
                .child(Framebuffer::cached(self.frame.clone()))
                .child(div().absolute().w(px(12.)).h(px(12.)).bg(rgb(0xffffff)))
        }
    }
    #[::core::prelude::v1::test]
    #[ignore = "requires a graphical desktop and a GPU; tests real GPUI scene reuse"]
    fn cursor_redraw_reuses_desktop_but_new_frames_and_resize_repaint() {
        let paints = Arc::new(AtomicUsize::new(0));
        let output = paints.clone();
        let renders = Arc::new(AtomicUsize::new(0));
        let root_renders = renders.clone();
        Application::new().run(move |cx| {
            let window = cx
                .open_window(
                    WindowOptions {
                        focus: false,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(400.), px(240.)),
                        ))),
                        titlebar: Some(TitlebarOptions {
                            title: Some("rumstsc render regression".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    move |_, cx| {
                        let frame = cx.new(|_| Framebuffer {
                            desktop: (1920, 1080),
                            tiles: vec![DesktopTile {
                                left: 0,
                                top: 0,
                                width: 1920,
                                height: 1080,
                                image: Arc::new(RenderImage::new(vec![image::Frame::new(
                                    image::RgbaImage::from_pixel(
                                        1920,
                                        1080,
                                        image::Rgba([20, 40, 80, 255]),
                                    ),
                                )])),
                            }],
                            first_paint: Some(Arc::new(AtomicBool::new(false))),
                            paints,
                            ..Default::default()
                        });
                        cx.new(|_| Harness {
                            frame,
                            width: 320.,
                            renders,
                        })
                    },
                )
                .unwrap();
            cx.spawn(async move |cx| {
                Timer::after(std::time::Duration::from_millis(250)).await;
                let view = window.update(cx, |_, _, cx| cx.entity()).unwrap();
                let initial = output.load(Ordering::Relaxed);
                let roots = root_renders.load(Ordering::Relaxed);
                assert!(initial > 0, "desktop must actually be painted");
                for _ in 0..25 {
                    cx.update(|cx| view.update(cx, |_, cx| cx.notify()))
                        .unwrap();
                    Timer::after(std::time::Duration::from_millis(20)).await;
                }
                let reused = output.load(Ordering::Relaxed);
                assert!(
                    root_renders.load(Ordering::Relaxed) > roots,
                    "cursor redraws must actually render the parent"
                );
                assert_eq!(reused, initial, "cursor-only redraws repainted desktop");
                cx.update(|cx| {
                    view.update(cx, |view, cx| {
                        view.frame.update(cx, |_, cx| cx.notify());
                        cx.notify();
                    })
                })
                .unwrap();
                Timer::after(std::time::Duration::from_millis(100)).await;
                let updated = output.load(Ordering::Relaxed);
                assert!(updated > reused, "a changed desktop must repaint");
                for index in 0..30 {
                    cx.update(|cx| {
                        view.update(cx, |view, cx| {
                            let retired = view.frame.update(cx, |frame, cx| {
                                let image = Arc::new(RenderImage::new(vec![image::Frame::new(
                                    image::RgbaImage::from_pixel(
                                        1920,
                                        1080,
                                        image::Rgba([index, 40, 80, 255]),
                                    ),
                                )]));
                                let retired = std::mem::replace(&mut frame.tiles[0].image, image);
                                // Same order as snapshot processing: notify the cached
                                // view before the next draw, outside parent render.
                                cx.notify();
                                retired
                            });
                            // Keep the window in App.windows, as in the network poll,
                            // so this really evicts the atlas texture on Linux.
                            cx.drop_image(retired, None);
                            cx.notify();
                        });
                    })
                    .unwrap();
                    Timer::after(std::time::Duration::from_millis(30)).await;
                }
                let replaced = output.load(Ordering::Relaxed);
                assert!(replaced >= updated + 30, "replacement frames must repaint");
                cx.update(|cx| {
                    view.update(cx, |view, cx| {
                        view.width = 280.;
                        cx.notify();
                    })
                })
                .unwrap();
                Timer::after(std::time::Duration::from_millis(100)).await;
                assert!(
                    output.load(Ordering::Relaxed) > replaced,
                    "resizing must repaint cached desktop"
                );
                cx.update(|cx| cx.quit()).unwrap();
            })
            .detach();
        });
    }
}
