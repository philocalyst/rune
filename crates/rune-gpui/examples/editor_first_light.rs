//! Native editor-host fixture. Compiler checks are not a working-window claim.
//! No Lisp Context or real platform IME handler is installed by this example.
use gpui::{App, Application, WindowOptions, prelude::*, px, size};
use rune_display::{FixedMetrics, Rect, host_updates};
use rune_editor::{Command, Document, RopeDocument, Session};
use rune_gpui::{
    FrameView, NativeFont,
    editor_bridge::EditorBridge,
    editor_transport::{OwnerEvent, pending_editor_transport},
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
        let (publisher,mut updates)=host_updates();let(sender,replies,owner)=pending_editor_transport(128,publisher.clone());
        let failure=Arc::new(Mutex::new(None::<(String,Vec<OwnerEvent>)>));let report=failure.clone();
        std::thread::spawn(move||{
            // The host is built only here. Runtime applications replace this pure
            // Session with with_runtime_owner's borrowed host inside its callback.
            let keepalive=publisher;let mut unbound=Some(owner);
            let result=(||->Result<(),(String,Vec<OwnerEvent>)>{
                let mut session=Session::new(RopeDocument::empty(),80,24).map_err(|e|(e.to_string(),vec![]))?;
                session.execute(Command::Insert("Native Rune editor host\n\nType text. Arrows, backspace, C-z/C-r.\nC-x 2 / C-x 3 splits; C-x o selects.\n".into()))
                    .map_err(|e|(e.to_string(),vec![]))?;
                session.execute(Command::GoTo(0)).map_err(|e|(e.to_string(),vec![]))?;
                let bridge=EditorBridge::new(session,metrics,bounds).map_err(|e|(format!("{e:?}"),vec![]))?;
                let worker=unbound.take().unwrap().bind_initialized(bridge)
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
                            FrameView::new_editor(frame,font,sender,replies,quit,
                                Rc::new(|e|panic!("editor input requires recovery: {e:?}")),
                                Rc::new(|ack|{if let Err(error)=ack.outcome{eprintln!("composition rejected: {error:?}");}}),cx)
                        });
                        window.focus(&view.read(cx).focus_handle());view
                    }).expect("native editor window creation");cx.activate(true);
            });
        }).detach();
    });
}
