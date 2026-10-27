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
struct State {
    shared: Arc<Shared>,
    buffer: Buffer,
    keys: Keys,
    parent: HWND,
    last_bounds: Bounds,
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
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
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
