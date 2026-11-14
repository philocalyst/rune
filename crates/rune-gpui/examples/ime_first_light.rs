//! Native platform-IME fixture. API checks are not native behavior tests.
//! Uses pure owner storage, not the live Lisp gateway.
use gpui::{App, Application, WindowOptions, prelude::*, px, size};
use rune_display::{FixedMetrics, Rect, host_updates};
use rune_editor::{Command, Document, RopeDocument, Session};
use rune_gpui::{
    FrameView, NativeFont,
    editor_bridge::EditorBridge,
    editor_transport::{OwnerEvent, pending_editor_transport},
    platform_context::platform_frames,
};
use std::{
    rc::Rc,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
fn main() {
    Application::new().run(|cx:&mut App|{
        cx.on_window_closed(|cx|{if cx.windows().is_empty(){cx.quit();}}).detach();
        let bounds=Rect{x:0.,y:0.,width:900.,height:620.};
        let font=NativeFont::measure(gpui::font("monospace"),17.,3.,cx).expect("native font calibration");
        let metrics=FixedMetrics(font.metrics());let quit=Arc::new(AtomicBool::new(false));
        let (publisher,mut updates)=host_updates();let(platform_publisher,platform_reader)=platform_frames();let(sender,replies,owner)=pending_editor_transport(128,publisher.clone());
        let failure=Arc::new(Mutex::new(None::<(String,Vec<OwnerEvent>)>));let report=failure.clone();
        std::thread::spawn(move||{
            // The host is built only here. Runtime applications replace this pure
            // Session with with_runtime_owner's borrowed host inside its callback.
            let keepalive=publisher;let mut unbound=Some(owner);
            let result=(||->Result<(),(String,Vec<OwnerEvent>)>{
                let mut session=Session::new(RopeDocument::empty(),80,24).map_err(|e|(e.to_string(),vec![]))?;
                session.execute(Command::Insert("Native Rune IME host\n\nPlatform text and UI-only preedit.\nArrows, backspace, C-z/C-r; fixed native commands.\nTyped C-x prefixes belong to non-IME mode.\n".into()))
                    .map_err(|e|(e.to_string(),vec![]))?;
                session.execute(Command::GoTo(0)).map_err(|e|(e.to_string(),vec![]))?;
                let bridge=EditorBridge::new(session,metrics,bounds).map_err(|e|(format!("{e:?}"),vec![]))?;
                let worker=unbound.take().unwrap().bind_ime_initialized(bridge,platform_publisher)
                    .map_err(|e|(format!("{:?}",e.error),e.undelivered))?;
                worker.run().map_err(|e|(format!("{:?}",e.error),e.undelivered))
            })();
            if let Err((error,mut rejected))=result{
                if let Some(owner)=unbound.take(){rejected.extend(owner.reject_unbound());}
                // Store failure before final publisher drop wakes the UI. No
                // polling, blocking UI receive, or infinite first-frame await.
                *report.lock().expect("startup failure slot")=Some((error,rejected));
            }
            drop(keepalive);
        });
        cx.spawn(async move|cx|{
            let frame=loop{match updates.next().await{
                Some(update)=>if let Some(frame)=update.frame{break frame;},
                None=>{
                    let error=failure.lock().expect("startup failure slot").take();
                    eprintln!("native editor startup failed: {error:?}");let _=cx.update(|cx|cx.quit());return;
                }
            }};
            let _=cx.update(move|cx|{
                cx.open_window(WindowOptions{window_bounds:Some(gpui::WindowBounds::Windowed(
                    gpui::Bounds::centered(None,size(px(bounds.width),px(bounds.height)),cx))),..Default::default()},
                    move|window,cx|{
                        let view=cx.new(|cx|{
                            FrameView::watch_updates(updates,cx).detach();
                            FrameView::new_ime(frame,font,sender,replies,platform_reader,quit,
                                Rc::new(|e|panic!("editor input requires recovery: {e:?}")),
                                Rc::new(|_|{}),Rc::new(|error|eprintln!("IME input requires resolution: {error:?}")),window,cx).expect("prepared IME context")
                        });
                        window.focus(&view.read(cx).focus_handle());view
                    }).expect("native editor window creation");cx.activate(true);
            });
        }).detach();
    });
}
