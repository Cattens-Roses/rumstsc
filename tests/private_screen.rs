//! Manual check on the same GPUI window type used by the application.
#[cfg(windows)]
#[test]
#[ignore = "requires an interactive Windows GPU/DWM desktop"]
fn private_screen_on_gpui_window() {
    use gpui::{prelude::*, *};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::sync::{Arc, Mutex};
    struct TestView;
    impl Render for TestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
    let result = Arc::new(Mutex::new(None));
    let output = result.clone();
    Application::new().run(move |cx| {
        cx.open_window(
            WindowOptions {
                show: false,
                focus: false,
                ..Default::default()
            },
            move |window, cx| {
                let checked = (|| -> std::result::Result<(), String> {
                    let handle = window.window_handle().map_err(|e| e.to_string())?;
                    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                        return Err("Not a Windows window".into());
                    };
                    let parent = rumstsc_display::Parent::Windows(handle.hwnd.get());
                    rumstsc_display::set_private_screen(parent, true)?;
                    rumstsc_display::set_private_screen(parent, false)
                })();
                *output.lock().unwrap() = Some(checked);
                cx.new(|_| TestView)
            },
        )
        .expect("create our unshown GPUI test window");
        cx.quit();
    });
    result
        .lock()
        .unwrap()
        .take()
        .expect("test executed")
        .unwrap();
}
