use super::*;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::w,
};
const UPDATE: u32 = WM_APP + 7;
pub(super) fn set_private_screen(window: isize, enabled: bool) -> Result<(), String> {
    unsafe {
        let window = HWND(window as *mut _);
        let affinity = if enabled {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        SetWindowDisplayAffinity(window, affinity)
            .map_err(|e| format!("Windows capture exclusion: {e}"))?;
        let mut actual = 0;
        if let Err(error) = GetWindowDisplayAffinity(window, &mut actual) {
            let _ = SetWindowDisplayAffinity(window, WDA_NONE);
            return Err(format!("Cannot verify capture exclusion: {error}"));
        }
        if actual != affinity.0 {
            let _ = SetWindowDisplayAffinity(window, WDA_NONE);
            return Err("Windows did not enable the requested capture exclusion".into());
        }
        Ok(())
    }
}
struct State {
    shared: Arc<Shared>,
    buffer: Buffer,
    keys: Keys,
    parent: HWND,
    last_bounds: Bounds,
    cursor: Option<HCURSOR>,
    cursor_hidden: bool,
}
pub(super) fn run(
    shared: Arc<Shared>,
    parent: isize,
    input: SyncSender<Input>,
) -> Result<(), String> {
    unsafe { run_inner(shared, parent, input).map_err(|e| e.to_string()) }
}
unsafe fn run_inner(
    shared: Arc<Shared>,
    parent: isize,
    input: SyncSender<Input>,
) -> windows::core::Result<()> {
    unsafe {
        let module = GetModuleHandleW(None)?;
        let class = w!("RumstscNativeFramebuffer");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: module.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let parent = HWND(parent as *mut _);
        let mut state = Box::new(State {
            shared: shared.clone(),
            buffer: Buffer::default(),
            keys: Keys::new(input),
            parent,
            last_bounds: Bounds::default(),
            cursor: None,
            cursor_hidden: false,
        });
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!(""),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            Some(parent),
            None,
            Some(module.into()),
            Some((&mut *state as *mut State).cast()),
        )?;
        let target = window.0 as isize;
        *shared.wake.lock().unwrap() = Some(Box::new(move || {
            let _ = PostMessageW(Some(HWND(target as *mut _)), UPDATE, WPARAM(0), LPARAM(0));
        }));
        let _ = PostMessageW(Some(window), UPDATE, WPARAM(0), LPARAM(0));
        let mut message = MSG::default();
        loop {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status == 0 {
                break;
            }
            if status == -1 {
                let _ = DestroyWindow(window);
                return Err(windows::core::Error::from_win32());
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        state.keys.release();
        *shared.wake.lock().unwrap() = None;
        Ok(())
    }
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut State;
        if pointer.is_null() {
            return DefWindowProcW(window, message, wparam, lparam);
        }
        let state = &mut *pointer;
        match message {
            UPDATE => {
                if state.shared.stop.load(Ordering::Acquire) {
                    let _ = DestroyWindow(window);
                    return LRESULT(0);
                }
                if let Err(error) = state.buffer.receive(&state.shared) {
                    *state.shared.error.lock().unwrap() = Some(error);
                    let _ = DestroyWindow(window);
                    return LRESULT(0);
                }
                if let Some(pointer) = state.buffer.pointer.take() {
                    match make_cursor(&pointer) {
                        Ok(cursor) => {
                            let previous = std::mem::replace(&mut state.cursor, cursor);
                            state.cursor_hidden = matches!(pointer, Pointer::Hidden);
                            let mut position = POINT::default();
                            if GetCursorPos(&mut position).is_ok()
                                && WindowFromPoint(position) == window
                            {
                                SetCursor(if state.cursor_hidden {
                                    None
                                } else {
                                    state.cursor.or_else(|| LoadCursorW(None, IDC_ARROW).ok())
                                });
                            }
                            if let Some(previous) = previous.filter(|c| !c.0.is_null()) {
                                let _ = DestroyCursor(previous);
                            }
                        }
                        Err(error) => *state.shared.error.lock().unwrap() = Some(error.to_string()),
                    }
                }
                let b = state.buffer.bounds;
                if state.last_bounds != b {
                    let _ = SetWindowPos(
                        window,
                        None,
                        b.x,
                        b.y,
                        b.width.min(8192) as i32,
                        b.height.min(8192) as i32,
                        SWP_NOACTIVATE
                            | SWP_NOZORDER
                            | if b.width > 0 && b.height > 0 {
                                SWP_SHOWWINDOW
                            } else {
                                SWP_HIDEWINDOW
                            },
                    );
                    state.last_bounds = b;
                }
                let _ = InvalidateRect(Some(window), None, false);
                LRESULT(0)
            }
            WM_SETCURSOR if lparam.0 as u16 == HTCLIENT as u16 => {
                SetCursor(if state.cursor_hidden {
                    None
                } else {
                    state.cursor.or_else(|| LoadCursorW(None, IDC_ARROW).ok())
                });
                LRESULT(1)
            }
            WM_PAINT => {
                let _span = state.shared.trace.span(
                    "native_present_submit",
                    "native",
                    state.buffer.pixels.len() as u64,
                );
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(window, &mut paint);
                let mut client = RECT::default();
                let _ = GetClientRect(window, &mut client);
                paint_frame(dc, &client, &state.buffer);
                state.shared.presented(&state.buffer);
                let _ = EndPaint(window, &paint);
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
            | WM_MBUTTONUP | WM_MOUSEMOVE => {
                let x = lparam.0 as u16 as i16 as i32;
                let y = (lparam.0 >> 16) as u16 as i16 as i32;
                if message == WM_MOUSEMOVE {
                    state
                        .shared
                        .hover_controls(&state.buffer, x, y, state.keys.buttons);
                }
                let flag = match message {
                    WM_LBUTTONDOWN => 0x9000,
                    WM_LBUTTONUP => 0x1000,
                    WM_RBUTTONDOWN => 0xa000,
                    WM_RBUTTONUP => 0x2000,
                    WM_MBUTTONDOWN => 0xc000,
                    WM_MBUTTONUP => 0x4000,
                    _ => 0x800,
                };
                if flag & 0x8000 != 0 {
                    state.shared.captured.store(true, Ordering::Release);
                    let _ = SetFocus(Some(window));
                    SetCapture(window);
                }
                state.keys.mouse(&state.buffer, x, y, flag);
                if state.keys.buttons == 0 {
                    let _ = ReleaseCapture();
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let mut point = POINT {
                    x: lparam.0 as u16 as i16 as i32,
                    y: (lparam.0 >> 16) as u16 as i16 as i32,
                };
                let _ = ScreenToClient(window, &mut point);
                let delta = (wparam.0 >> 16) as u16 as i16;
                let rotation = delta.clamp(-255, 255);
                let flag = if message == WM_MOUSEWHEEL {
                    0x0200
                } else {
                    0x0400
                } | (rotation as u16 & 0x1ff);
                state.keys.mouse(&state.buffer, point.x, point.y, flag);
                LRESULT(0)
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let code = ((lparam.0 >> 16) & 0xff) as u16;
                let extended = lparam.0 & (1 << 24) != 0;
                let released = message == WM_KEYUP || message == WM_SYSKEYUP;
                if code == 0x47
                    && extended
                    && !released
                    && state.keys.held.iter().any(|k| k.0 == 0x1d)
                    && state.keys.held.iter().any(|k| k.0 == 0x38)
                {
                    state.keys.release();
                    let _ = SetFocus(Some(state.parent));
                    return LRESULT(0);
                }
                if code != 0 {
                    state.keys.key(code, extended, released);
                }
                LRESULT(0)
            }
            WM_SETFOCUS => {
                state.shared.captured.store(true, Ordering::Release);
                LRESULT(0)
            }
            WM_KILLFOCUS => {
                state.shared.captured.store(false, Ordering::Release);
                state.keys.release();
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_CLOSE => {
                let _ = DestroyWindow(window);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(cursor) = state.cursor.take() {
                    let _ = DestroyCursor(cursor);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

unsafe fn make_cursor(pointer: &Pointer) -> windows::core::Result<Option<HCURSOR>> {
    let Pointer::Image(image) = pointer else {
        return Ok(None);
    };
    unsafe {
        let width = image.width as usize;
        let height = image.height as usize;
        let stride = width.div_ceil(16) * 2;
        let mut mask = vec![0u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                if image.bgra[(y * width + x) * 4 + 3] == 0 || image.invert[y * width + x] {
                    mask[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        // Windows' monochrome cursor format preserves screen inversion, used
        // by classic text carets and some resize arrows: AND rows, then XOR.
        if image.invert.iter().any(|p| *p) {
            let mut mono = mask.clone();
            mono.resize(stride * height * 2, 0);
            for y in 0..height {
                for x in 0..width {
                    let pixel = &image.bgra[(y * width + x) * 4..][..4];
                    if pixel[3] != 0 && pixel[..3].iter().map(|v| *v as u16).sum::<u16>() >= 384 {
                        mono[stride * height + y * stride + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            let bitmap = CreateBitmap(
                width as i32,
                (height * 2) as i32,
                1,
                1,
                Some(mono.as_ptr().cast()),
            );
            if bitmap.is_invalid() {
                return Err(windows::core::Error::from_win32());
            }
            let result = CreateIconIndirect(&ICONINFO {
                fIcon: false.into(),
                xHotspot: image.hot_x.into(),
                yHotspot: image.hot_y.into(),
                hbmMask: bitmap,
                ..Default::default()
            });
            let _ = DeleteObject(bitmap.into());
            return result.map(|icon| Some(HCURSOR(icon.0)));
        }
        let mask_bitmap = CreateBitmap(
            width as i32,
            height as i32,
            1,
            1,
            Some(mask.as_ptr().cast()),
        );
        if mask_bitmap.is_invalid() {
            return Err(windows::core::Error::from_win32());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = match CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(color) => color,
            Err(error) => {
                let _ = DeleteObject(mask_bitmap.into());
                return Err(error);
            }
        };
        std::ptr::copy_nonoverlapping(image.bgra.as_ptr(), bits.cast(), image.bgra.len());
        let result = CreateIconIndirect(&ICONINFO {
            fIcon: false.into(),
            xHotspot: image.hot_x.into(),
            yHotspot: image.hot_y.into(),
            hbmMask: mask_bitmap,
            hbmColor: color,
        });
        let _ = DeleteObject(mask_bitmap.into());
        let _ = DeleteObject(color.into());
        result.map(|icon| Some(HCURSOR(icon.0)))
    }
}

unsafe fn paint_frame(dc: HDC, client: &RECT, buffer: &Buffer) {
    unsafe {
        let b = buffer.viewport();
        let black = HBRUSH(GetStockObject(BLACK_BRUSH).0);
        if b.width == 0 || b.height == 0 {
            FillRect(dc, client, black);
        } else {
            for rect in [
                RECT {
                    left: 0,
                    top: 0,
                    right: client.right,
                    bottom: b.y,
                },
                RECT {
                    left: 0,
                    top: b.y + b.height as i32,
                    right: client.right,
                    bottom: client.bottom,
                },
                RECT {
                    left: 0,
                    top: b.y,
                    right: b.x,
                    bottom: b.y + b.height as i32,
                },
                RECT {
                    left: b.x + b.width as i32,
                    top: b.y,
                    right: client.right,
                    bottom: b.y + b.height as i32,
                },
            ] {
                if rect.left < rect.right && rect.top < rect.bottom {
                    FillRect(dc, &rect, black);
                }
            }
        }
        if b.width > 0 && b.height > 0 && !buffer.pixels.is_empty() {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: buffer.width as i32,
                    biHeight: -(buffer.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            SetStretchBltMode(dc, COLORONCOLOR);
            StretchDIBits(
                dc,
                b.x,
                b.y,
                b.width as i32,
                b.height as i32,
                0,
                0,
                buffer.width as i32,
                buffer.height as i32,
                Some(buffer.pixels.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an interactive Windows DWM desktop"]
    fn private_screen_affinity_enables_and_disables_on_our_own_window() {
        unsafe {
            // An unshown, process-owned top-level test window. No user windows
            // or monitoring software are modified by this integration check.
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("rumstsc capture exclusion test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                64,
                64,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let result = (|| -> Result<(), String> {
                for enabled in [true, false] {
                    set_private_screen(window.0 as isize, enabled)?;
                    let mut actual = 0;
                    GetWindowDisplayAffinity(window, &mut actual).map_err(|e| e.to_string())?;
                    assert_eq!(
                        actual,
                        if enabled {
                            WDA_EXCLUDEFROMCAPTURE.0
                        } else {
                            WDA_NONE.0
                        }
                    );
                }
                Ok(())
            })();
            let _ = DestroyWindow(window);
            result.unwrap();
        }
    }
    #[test]
    fn server_cursor_resource_preserves_hotspot() {
        unsafe {
            for invert in [false, true] {
                let pointer = Pointer::Image(Arc::new(rumstsc_rdp::pointer::PointerImage {
                    width: 2,
                    height: 2,
                    hot_x: 1,
                    hot_y: 0,
                    bgra: vec![255; 16],
                    invert: vec![invert; 4],
                }));
                let cursor = make_cursor(&pointer).unwrap().unwrap();
                let mut info = ICONINFO::default();
                GetIconInfo(HICON(cursor.0), &mut info).unwrap();
                assert!(!info.fIcon.as_bool());
                assert_eq!((info.xHotspot, info.yHotspot), (1, 0));
                let _ = DeleteObject(info.hbmMask.into());
                if !info.hbmColor.is_invalid() {
                    let _ = DeleteObject(info.hbmColor.into());
                }
                DestroyCursor(cursor).unwrap();
            }
            assert!(make_cursor(&Pointer::Default).unwrap().is_none());
            assert!(make_cursor(&Pointer::Hidden).unwrap().is_none());
        }
    }
    #[test]
    fn gdi_paints_bgra_without_a_visible_window() {
        unsafe {
            let dc = CreateCompatibleDC(None);
            assert!(!dc.is_invalid());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 2,
                    biHeight: -1,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0).unwrap();
            let old = SelectObject(dc, bitmap.into());
            let buffer = Buffer {
                pointer: None,
                upscale: false,
                width: 2,
                height: 1,
                pixels: vec![10, 20, 30, 255, 40, 50, 60, 255],
                bounds: Bounds {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
            };
            paint_frame(
                dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 1,
                },
                &buffer,
            );
            let _ = GdiFlush();
            let data = std::slice::from_raw_parts(pixels.cast::<u8>(), 8);
            assert_eq!(&data[..3], &[10, 20, 30]);
            assert_eq!(&data[4..7], &[40, 50, 60]);
            SelectObject(dc, old);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
        }
    }
}
