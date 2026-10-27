use super::*;
use std::sync::Arc;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME,
    connection::{Connection, RequestConnection},
    protocol::{Event, xproto::*},
    rust_connection::RustConnection,
};
fn scancode(key: u8) -> Option<(u16, bool)> {
    let key = key.checked_sub(8)?;
    Some(match key {
        1..=83 => (key as u16, false),
        86 => (0x56, false),
        87 => (0x57, false),
        88 => (0x58, false),
        96 => (0x1c, true),
        97 => (0x1d, true),
        98 => (0x35, true),
        99 => (0x37, true),
        100 => (0x38, true),
        102 => (0x47, true),
        103 => (0x48, true),
        104 => (0x49, true),
        105 => (0x4b, true),
        106 => (0x4d, true),
        107 => (0x4f, true),
        108 => (0x50, true),
        109 => (0x51, true),
        110 => (0x52, true),
        111 => (0x53, true),
        125 => (0x5b, true),
        126 => (0x5c, true),
        127 => (0x5d, true),
        _ => return None,
    })
}
// GPUI 0.2.2 does not implement HasWindowHandle for its X11Window. Resolve
// our own process window through the standard EWMH PID/title properties instead.
// Never attach to a window belonging to another process or an ambiguous match.
pub(super) fn run_process(
    shared: Arc<Shared>,
    pid: u32,
    input: SyncSender<Input>,
) -> Result<(), String> {
    let parent = find_parent(pid).map_err(|e| e.to_string())?;
    run(shared, parent, input)
}
fn find_parent(pid: u32) -> Result<u32, Box<dyn std::error::Error>> {
    let (connection, screen) = RustConnection::connect(None)?;
    let pid_atom = connection.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
    let name_atom = connection
        .intern_atom(false, b"_NET_WM_NAME")?
        .reply()?
        .atom;
    let mut pending = vec![(connection.setup().roots[screen].root, 0)];
    let mut found = vec![];
    let mut visited = 0;
    while let Some((window, depth)) = pending.pop() {
        visited += 1;
        if visited > 4096 {
            return Err("X11 parent search limit".into());
        }
        if let Ok(property) = connection
            .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)?
            .reply()
            && property.value32().and_then(|mut v| v.next()) == Some(pid)
        {
            let name = connection
                .get_property(false, window, name_atom, AtomEnum::ANY, 0, 256)?
                .reply()?;
            let legacy = connection
                .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 256)?
                .reply()?;
            if name.value.starts_with(b"rumstsc") || legacy.value.starts_with(b"rumstsc") {
                found.push(window);
            }
        }
        if depth < 2
            && let Ok(tree) = connection.query_tree(window)?.reply()
        {
            pending.extend(tree.children.into_iter().map(|id| (id, depth + 1)));
        }
    }
    if found.len() != 1 {
        return Err(
            "Cannot identify this process's X11 desktop parent; use GPUI or an X11 session".into(),
        );
    }
    Ok(found[0])
}
pub(super) fn set_application_icon(
    pid: u32,
    argb: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::wrapper::ConnectionExt as _;
    let window = find_parent(pid)?;
    let (connection, _) = RustConnection::connect(None)?;
    let property = connection
        .intern_atom(false, b"_NET_WM_ICON")?
        .reply()?
        .atom;
    let pixels: Vec<u32> = argb
        .chunks_exact(4)
        .map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap()))
        .collect();
    connection
        .change_property32(
            PropMode::REPLACE,
            window,
            property,
            AtomEnum::CARDINAL,
            &pixels,
        )?
        .check()?;
    connection.flush()?;
    Ok(())
}
pub(super) fn run(
    shared: Arc<Shared>,
    parent: u32,
    input: SyncSender<Input>,
) -> Result<(), String> {
    run_inner(shared, parent, input).map_err(|e| e.to_string())
}
fn run_inner(
    shared: Arc<Shared>,
    parent: u32,
    input: SyncSender<Input>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (connection, _) = RustConnection::connect(None)?;
    let connection = Arc::new(connection);
    let geometry = connection.get_geometry(parent)?.reply()?;
    if ![24, 32].contains(&geometry.depth)
        || connection.setup().image_byte_order != ImageOrder::LSB_FIRST
    {
        return Err("Native X11 display needs a little-endian 24/32-bit visual".into());
    }
    let window = connection.generate_id()?;
    let gc = connection.generate_id()?;
    connection
        .create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0).event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::FOCUS_CHANGE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
        )?
        .check()?;
    connection
        .create_gc(
            gc,
            window,
            &CreateGCAux::new().foreground(0).graphics_exposures(0),
        )?
        .check()?;
    let atom = connection
        .intern_atom(false, b"RUMSTSC_FRAMEBUFFER_UPDATE")?
        .reply()?
        .atom;
    let wake_connection = connection.clone();
    *shared.wake.lock().unwrap() = Some(Box::new(move || {
        let event = ClientMessageEvent::new(32, window, atom, [0u32; 5]);
        let _ = wake_connection.send_event(false, window, EventMask::NO_EVENT, event);
        let _ = wake_connection.flush();
    }));
    let mut buffer = Buffer::default();
    buffer.receive(&shared)?;
    let mut keys = Keys::new(input);
    let mut scaled = vec![];
    let mut pixmap = None;
    let mut old_bounds = Bounds::default();
    let mut repaint = true;
    let mut cursor = None;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        loop {
            if shared.stop.load(Ordering::Acquire) {
                break;
            }
            if let Some(pointer) = buffer.pointer.take() {
                let next = make_cursor(&connection, window, &pointer)?;
                connection.change_window_attributes(
                    window,
                    &ChangeWindowAttributesAux::new().cursor(next.unwrap_or(0)),
                )?;
                if let Some(previous) = cursor.replace(next.unwrap_or(0)).filter(|id| *id != 0) {
                    connection.free_cursor(previous)?;
                }
            }
            let bounds = buffer.bounds;
            if bounds != old_bounds {
                connection.configure_window(
                    window,
                    &ConfigureWindowAux::new()
                        .x(bounds.x)
                        .y(bounds.y)
                        .width(bounds.width.clamp(1, 8192))
                        .height(bounds.height.clamp(1, 8192)),
                )?;
                if bounds.width > 0 && bounds.height > 0 {
                    connection.map_window(window)?;
                } else {
                    connection.unmap_window(window)?;
                }
                connection.clear_area(false, window, 0, 0, 0, 0)?;
                old_bounds = bounds;
                repaint = true;
            }
            if repaint {
                let _span = shared.trace.span(
                    "native_present_submit",
                    "native",
                    buffer.pixels.len() as u64,
                );
                paint(
                    &connection,
                    window,
                    gc,
                    geometry.depth,
                    &buffer,
                    &mut scaled,
                    &mut pixmap,
                )?;
                shared.presented(&buffer);
                repaint = false;
            }
            connection.flush()?;
            // A client message wakes this blocking read when the UI sends new tiles.
            // Expose/input events are processed on this same native display thread.
            let event = connection.wait_for_event()?;
            match event {
                Event::ClientMessage(event) if event.type_ == atom => {
                    buffer.receive(&shared)?;
                    repaint = true;
                }
                Event::Expose(_) => {
                    if let Some((id, w, h)) = pixmap {
                        let v = buffer.viewport();
                        connection.copy_area(id, window, gc, 0, 0, v.x as i16, v.y as i16, w, h)?;
                    } else {
                        repaint = true;
                    }
                }
                Event::DestroyNotify(_) => break,
                Event::FocusIn(event)
                    if event.mode == NotifyMode::NORMAL
                        || event.mode == NotifyMode::WHILE_GRABBED =>
                {
                    shared.captured.store(true, Ordering::Release);
                }
                Event::FocusOut(event)
                    if event.mode == NotifyMode::NORMAL
                        || event.mode == NotifyMode::WHILE_GRABBED =>
                {
                    shared.captured.store(false, Ordering::Release);
                    keys.release();
                }
                Event::KeyPress(event) => {
                    if let Some((code, extended)) = scancode(event.detail) {
                        if code == 0x47
                            && extended
                            && keys.held.iter().any(|k| k.0 == 0x1d)
                            && keys.held.iter().any(|k| k.0 == 0x38)
                        {
                            keys.release();
                            connection.set_input_focus(InputFocus::PARENT, parent, CURRENT_TIME)?;
                        } else {
                            keys.key(code, extended, false);
                        }
                    }
                }
                Event::KeyRelease(event) => {
                    if let Some((code, extended)) = scancode(event.detail) {
                        keys.key(code, extended, true);
                    }
                }
                Event::MotionNotify(event) => {
                    keys.mouse(&buffer, event.event_x as i32, event.event_y as i32, 0x800)
                }
                Event::ButtonPress(event) => {
                    if event.detail <= 3 {
                        shared.captured.store(true, Ordering::Release);
                        connection.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)?;
                    }
                    let flag = match event.detail {
                        1 => 0x9000,
                        2 => 0xc000,
                        3 => 0xa000,
                        4 => 0x0200 | 120,
                        5 => 0x0200 | 0x188,
                        6 => 0x0400 | 0x188,
                        7 => 0x0400 | 120,
                        _ => 0,
                    };
                    if flag != 0 {
                        keys.mouse(&buffer, event.event_x as i32, event.event_y as i32, flag);
                    }
                }
                Event::ButtonRelease(event) => {
                    let flag = match event.detail {
                        1 => 0x1000,
                        2 => 0x4000,
                        3 => 0x2000,
                        _ => 0,
                    };
                    if flag != 0 {
                        keys.mouse(&buffer, event.event_x as i32, event.event_y as i32, flag);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })();
    keys.release();
    *shared.wake.lock().unwrap() = None;
    if let Some((id, _, _)) = pixmap {
        let _ = connection.free_pixmap(id);
    }
    if let Some(cursor) = cursor.filter(|id| *id != 0) {
        let _ = connection.free_cursor(cursor);
    }
    let _ = connection.free_gc(gc);
    let _ = connection.destroy_window(window);
    let _ = connection.flush();
    result
}
// XRender cursors preserve color/alpha and move independently of framebuffer paints.
fn make_cursor(
    connection: &RustConnection,
    window: u32,
    pointer: &Pointer,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    use x11rb::protocol::render::{ConnectionExt as _, CreatePictureAux, PictType};
    if matches!(pointer, Pointer::Default) {
        return Ok(None);
    }
    let (width, height, hot_x, hot_y, mut pixels) = match pointer {
        Pointer::Image(image) => (
            image.width,
            image.height,
            image.hot_x,
            image.hot_y,
            image.bgra.clone(),
        ),
        _ => (1, 1, 0, 0, vec![0; 4]),
    };
    // XRender uses premultiplied ARGB32 (BGRA bytes on this little-endian visual).
    for pixel in pixels.chunks_exact_mut(4) {
        for c in 0..3 {
            pixel[c] = (pixel[c] as u16 * pixel[3] as u16 / 255) as u8;
        }
    }
    let formats = connection.render_query_pict_formats()?.reply()?;
    let format = formats
        .formats
        .iter()
        .find(|f| {
            f.type_ == PictType::DIRECT
                && f.depth == 32
                && f.direct.red_shift == 16
                && f.direct.green_shift == 8
                && f.direct.blue_shift == 0
                && f.direct.alpha_shift == 24
                && f.direct.red_mask == 255
                && f.direct.green_mask == 255
                && f.direct.blue_mask == 255
                && f.direct.alpha_mask == 255
        })
        .ok_or("XRender ARGB32 cursor format unavailable")?
        .id;
    let pixmap = connection.generate_id()?;
    let gc = connection.generate_id()?;
    let picture = connection.generate_id()?;
    let cursor = connection.generate_id()?;
    connection.create_pixmap(32, pixmap, window, width, height)?;
    connection.create_gc(gc, pixmap, &CreateGCAux::new())?;
    connection.put_image(
        ImageFormat::Z_PIXMAP,
        pixmap,
        gc,
        width,
        height,
        0,
        0,
        0,
        32,
        &pixels,
    )?;
    connection.render_create_picture(picture, pixmap, format, &CreatePictureAux::new())?;
    connection
        .render_create_cursor(cursor, picture, hot_x, hot_y)?
        .check()?;
    connection.render_free_picture(picture)?;
    connection.free_gc(gc)?;
    connection.free_pixmap(pixmap)?;
    Ok(Some(cursor))
}
fn paint(
    connection: &RustConnection,
    window: u32,
    gc: u32,
    depth: u8,
    buffer: &Buffer,
    scaled: &mut Vec<u8>,
    pixmap: &mut Option<(u32, u16, u16)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let v = buffer.viewport();
    if v.width == 0 || v.height == 0 || buffer.pixels.is_empty() {
        return Ok(());
    }
    let w = v.width as usize;
    let h = v.height as usize;
    scaled.resize(w * h * 4, 0);
    for y in 0..h {
        let sy = y * buffer.height as usize / h;
        for x in 0..w {
            let sx = x * buffer.width as usize / w;
            let src = (sy * buffer.width as usize + sx) * 4;
            let dst = (y * w + x) * 4;
            scaled[dst..dst + 4].copy_from_slice(&buffer.pixels[src..src + 4]);
        }
    }
    if pixmap
        .as_ref()
        .is_none_or(|(_, pw, ph)| *pw != w as u16 || *ph != h as u16)
    {
        if let Some((id, _, _)) = pixmap.take() {
            connection.free_pixmap(id)?;
        }
        let id = connection.generate_id()?;
        connection.create_pixmap(depth, id, window, w as u16, h as u16)?;
        *pixmap = Some((id, w as u16, h as u16));
    }
    let id = pixmap.unwrap().0;
    let rows = (connection.maximum_request_bytes().saturating_sub(32) / (w * 4))
        .max(1)
        .min(h);
    for y in (0..h).step_by(rows) {
        let count = rows.min(h - y);
        connection.put_image(
            ImageFormat::Z_PIXMAP,
            id,
            gc,
            w as u16,
            count as u16,
            0,
            y as i16,
            0,
            depth,
            &scaled[y * w * 4..(y + count) * w * 4],
        )?;
    }
    // Present the complete offscreen pixmap once, after every upload strip.
    connection.copy_area(
        id, window, gc, 0, 0, v.x as i16, v.y as i16, w as u16, h as u16,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::wrapper::ConnectionExt as _;
    #[test]
    fn xrender_server_cursor_and_hidden_cursor() {
        if std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let (connection, screen) = RustConnection::connect(None).unwrap();
        let root = connection.setup().roots[screen].root;
        let pointer = Pointer::Image(Arc::new(rumstsc_rdp::pointer::PointerImage {
            width: 2,
            height: 2,
            hot_x: 1,
            hot_y: 0,
            bgra: vec![
                0, 0, 0, 255, 255, 255, 255, 255, 20, 40, 60, 128, 0, 0, 0, 0,
            ],
            invert: vec![false; 4],
        }));
        for pointer in [pointer, Pointer::Hidden] {
            let cursor = make_cursor(&connection, root, &pointer).unwrap().unwrap();
            connection.free_cursor(cursor).unwrap().check().unwrap();
        }
        assert!(
            make_cursor(&connection, root, &Pointer::Default)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn native_x11_legacy_title_parent_and_pixels() {
        if std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let (connection, screen) = RustConnection::connect(None).unwrap();
        let root = &connection.setup().roots[screen];
        let visual = root
            .allowed_depths
            .iter()
            .find(|d| d.depth == 32)
            .and_then(|d| d.visuals.first());
        let (depth, visual) =
            visual.map_or((root.root_depth, root.root_visual), |v| (32, v.visual_id));
        let colormap = connection.generate_id().unwrap();
        connection
            .create_colormap(ColormapAlloc::NONE, colormap, root.root, visual)
            .unwrap();
        let parent = connection.generate_id().unwrap();
        connection
            .create_window(
                depth,
                parent,
                root.root,
                0,
                0,
                8,
                8,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &CreateWindowAux::new()
                    .background_pixel(0)
                    .border_pixel(0)
                    .colormap(colormap),
            )
            .unwrap()
            .check()
            .unwrap();
        let pid = connection
            .intern_atom(false, b"_NET_WM_PID")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        connection
            .change_property32(
                PropMode::REPLACE,
                parent,
                pid,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )
            .unwrap();
        connection
            .change_property8(
                PropMode::REPLACE,
                parent,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"rumstsc native test",
            )
            .unwrap();
        connection.map_window(parent).unwrap();
        connection.flush().unwrap();
        assert_eq!(find_parent(std::process::id()).unwrap(), parent);
        let (input, events) = std::sync::mpsc::sync_channel(16);
        let display = Display::start(
            Parent::X11Process(std::process::id()),
            input,
            Arc::new(rumstsc_rdp::trace::Trace::new(false)),
        )
        .unwrap();
        display.handle().bounds(Bounds {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        });
        display.submit(Snapshot {
            generation: 1,
            revision: 1,
            width: 2,
            height: 1,
            tiles: vec![TilePixels {
                index: 0,
                left: 0,
                top: 0,
                width: 2,
                height: 1,
                bgra: vec![10, 20, 30, 255, 40, 50, 60, 255],
            }],
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let child = loop {
            assert!(display.error().is_none());
            assert!(
                std::time::Instant::now() < deadline,
                "native pixel presentation timed out"
            );
            let children = connection
                .query_tree(parent)
                .unwrap()
                .reply()
                .unwrap()
                .children;
            if let Some(&child) = children.first() {
                let geometry = connection.get_geometry(child).unwrap().reply().unwrap();
                if geometry.width == 2
                    && geometry.height == 1
                    && connection
                        .get_window_attributes(child)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .map_state
                        == x11rb::protocol::xproto::MapState::VIEWABLE
                {
                    let pixels = connection
                        .get_image(ImageFormat::Z_PIXMAP, child, 0, 0, 2, 1, u32::MAX)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .data;
                    if pixels.len() == 8
                        && pixels[..3] == [10, 20, 30]
                        && pixels[4..7] == [40, 50, 60]
                    {
                        break child;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        connection
            .send_event(
                false,
                child,
                EventMask::BUTTON_PRESS,
                ButtonPressEvent {
                    response_type: BUTTON_PRESS_EVENT,
                    detail: 1,
                    root: root.root,
                    event: child,
                    same_screen: true,
                    ..Default::default()
                },
            )
            .unwrap();
        connection
            .send_event(
                false,
                child,
                EventMask::FOCUS_CHANGE,
                FocusOutEvent {
                    response_type: FOCUS_OUT_EVENT,
                    event: child,
                    mode: NotifyMode::GRAB,
                    ..Default::default()
                },
            )
            .unwrap();
        connection
            .send_event(
                false,
                child,
                EventMask::KEY_PRESS,
                KeyPressEvent {
                    response_type: KEY_PRESS_EVENT,
                    detail: 38,
                    root: root.root,
                    event: child,
                    same_screen: true,
                    ..Default::default()
                },
            )
            .unwrap();
        connection
            .send_event(
                false,
                child,
                EventMask::FOCUS_CHANGE,
                FocusOutEvent {
                    response_type: FOCUS_OUT_EVENT,
                    event: child,
                    mode: NotifyMode::NORMAL,
                    ..Default::default()
                },
            )
            .unwrap();
        connection.flush().unwrap();
        let timeout = std::time::Duration::from_secs(2);
        assert!(matches!(
            events.recv_timeout(timeout).unwrap(),
            Input::Mouse { flags: 0x9000, .. }
        ));
        assert!(matches!(
            events.recv_timeout(timeout).unwrap(),
            Input::Scancode {
                code: 0x1e,
                released: false,
                ..
            }
        ));
        assert!(matches!(
            events.recv_timeout(timeout).unwrap(),
            Input::Scancode {
                code: 0x1e,
                released: true,
                ..
            }
        ));
        assert!(matches!(
            events.recv_timeout(timeout).unwrap(),
            Input::Mouse { flags: 0x1000, .. }
        ));
        drop(display);
        let _ = connection.destroy_window(parent);
        let _ = connection.flush();
    }
}
