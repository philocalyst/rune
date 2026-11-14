//! Native first-light fixture. Requires a working GPUI platform/GPU toolchain.
//! This exercises a real native window, not interpreter/editor integration.
use gpui::prelude::*;
use gpui::{App, Application, WindowOptions, px, size};
use rune_display::*;
use rune_gpui::{FrameView, NativeFont};
use std::sync::{Arc, atomic::AtomicBool, mpsc};

fn main() {
    Application::new().run(|cx: &mut App| {
        // GPUI does not make a closed final window an application quit policy.
        // Use its official example's lifecycle hook, without retaining a view.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Rect { x: 0.0, y: 0.0, width: 900.0, height: 620.0 };
        let snapshot = Arc::new(StringSnapshot::new(
            "Rune native first light\n\nImmutable snapshot → layout → native canvas\n\tUnicode: λ 中 🙂\n", 1));
        let native_font = NativeFont::measure(gpui::font("monospace"), 17.0, 3.0, cx)
            .expect("native monospace font calibration");
        let metrics = FixedMetrics(native_font.metrics());
        let mut spec = WindowSpec::new(Rect { x: 16.0, y: 16.0, width: 868.0, height: 588.0 });
        spec.point = 5;
        let mut host = first_light_host::FirstLightHost::new(snapshot, spec, metrics, bounds, 16.0)
            .expect("fixture host");
        let frame = host.frame();
        let (publisher, updates) = host_updates();
        let (sender, receiver) = mpsc::sync_channel(128);
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if publisher.input_capacity_available().is_err() { break; }
                match host.apply(&event) {
                    Ok(true) => if publisher.publish(host.frame()).is_err() { break; },
                    Ok(false) => eprintln!("fixture does not handle: {event:?}"),
                    Err(error) => eprintln!("fixture event rejected: {error}"),
                }
            }
        });
        let quit = Arc::new(AtomicBool::new(false));
        let input = InputSender::new(sender, quit.clone());
        cx.open_window(
            WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(
                    gpui::Bounds::centered(None, size(px(bounds.width), px(bounds.height)), cx))),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| {
                    FrameView::watch_updates(updates, cx).detach();
                    FrameView::new(frame, native_font, input, quit, std::rc::Rc::new(|event| {
                    // Fixture fails loudly rather than silently losing input.
                    panic!("native input backpressure: {event:?}");
                }), cx)
                });
                window.focus(&view.read(cx).focus_handle());
                view
            },
        ).expect("native window creation");
        cx.activate(true);
    });
}
