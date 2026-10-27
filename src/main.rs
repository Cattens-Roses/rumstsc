#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod assets;
mod input;
mod keyboard;
mod settings;
mod theme;
use gpui::{prelude::*, *};
fn main() {
    let startup_path = settings::Settings::startup_trace_path();
    let startup_trace = std::sync::Arc::new(rumstsc_rdp::trace::Trace::new(startup_path.is_some()));
    startup_trace.mark("process_main", "ui", 0);
    if let Some(path) = startup_path {
        rumstsc_rdp::trace::Trace::autosave(&startup_trace, path);
    }
    let app = {
        let _span = startup_trace.span("gpui_init", "ui", 0);
        Application::new().with_assets(assets::Assets)
    };
    app.run(move |cx: &mut App| {
        let _span = startup_trace.span("window_open", "ui", 0);
        let window_trace = startup_trace.clone();
        let bounds = Bounds::centered(None, size(px(640.), px(840.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(500.), px(560.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("rumstsc — Remote Desktop".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                window.set_app_id("rumstsc");
                let client = cx.new(|cx| app::Client::new(window, cx, window_trace.clone()));
                client.read(cx).host.read(cx).focus.focus(window);
                client
            },
        )
        .expect("Unable to open GPUI window. Check your graphical session and graphics driver.");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
