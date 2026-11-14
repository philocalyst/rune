//! Opt-in shared-owner native editor. Window behavior remains unverified.
use crate::editor_document::{LispRuntimeHost, with_lisp_runtime_owner};
use crate::native_command::{NativeShell, Reports};
use gpui::{App, Application, WindowOptions, prelude::*, px, size};
use rune_display::{FixedMetrics, Rect, host_updates};
use rune_gpui::{
    FrameView, NativeFont,
    editor_bridge::EditorBridge,
    editor_transport::{OwnerEvent, pending_editor_transport},
    evaluation_quit::EvaluationQuit,
    platform_context::platform_frames,
};
use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
pub(crate) fn run(sources: Vec<String>) -> Result<(), ()> {
    if sources.len() > 64 || sources.iter().any(|s| s.len() > 65_536) {
        eprintln!("native startup permits at most 64 sources of at most 65536 bytes each");
        return Err(());
    }
    let failed = Arc::new(AtomicBool::new(false));
    let startup_failed = failed.clone();
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Rect { x: 0., y: 0., width: 900., height: 620. };
        let font = match NativeFont::measure(gpui::font("monospace"), 17., 3., cx) {
            Ok(font) => font,
            Err(error) => {
                eprintln!("native font calibration failed: {error}");
                startup_failed.store(true, Ordering::Relaxed);
                cx.quit();
                return;
            }
        };
        let metrics = FixedMetrics(font.metrics());
        let quit = Arc::new(AtomicBool::new(false));
        let (publisher, mut updates) = host_updates();
        let (platform_publisher, platform_reader) = platform_frames();
        let (sender, replies, owner) = pending_editor_transport(128, publisher.clone());
        for (index, source) in sources.into_iter().enumerate() {
            if let Err(error) =
                sender.try_send(OwnerEvent::Evaluate { request_id: index as u64, source })
            {
                eprintln!("native startup evaluation admission failed: {error:?}");
                startup_failed.store(true, Ordering::Relaxed);
                cx.quit();
                return;
            }
        }
        let failure = Arc::new(Mutex::new(None::<(String, Vec<OwnerEvent>)>));
        let report = failure.clone();
        let (reports, report_updates) = Reports::channel();
        let owner_reports = reports.clone();
        let evaluation_quit = EvaluationQuit::default();
        let owner_quit = evaluation_quit.clone();
        std::thread::spawn(move || {
            // Context, one rooted Env, and the shared editor authority are created
            // only here; nothing live crosses into the UI executor.
            let keepalive = publisher;
            let mut unbound = Some(owner);
            let result = (|| -> Result<(), (String, Vec<OwnerEvent>)> {
                with_lisp_runtime_owner(80, 24, |host| {
                    let bridge = EditorBridge::new(host, metrics, bounds)
                        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                    let owner = unbound.take().expect("owner binds once");
                    let worker = match owner.bind_ime_initialized(bridge, platform_publisher) {
                        Ok(worker) => worker,
                        Err(error) => {
                            *report.lock().expect("startup failure slot") =
                                Some((format!("{:?}", error.error), error.undelivered));
                            return Err(anyhow::anyhow!("native IME initialization failed"));
                        }
                    };
                    worker
                        .with_interruptible_evaluator(evaluate_source_with_quit, owner_quit)
                        .run_with_outcome(|step| {
                            if let Some(evaluation) = step.evaluation {
                                if let Err(error) = &evaluation.outcome {
                                    eprintln!(
                                        "native evaluation {}: {error}",
                                        evaluation.request_id
                                    );
                                }
                                owner_reports.evaluation(evaluation);
                            }
                            if let Err(error) = step.edit {
                                eprintln!("native owner input: {error:?}");
                                owner_reports.notice(format!("Owner input: {error:?}"));
                            }
                            if let Some(error) = step.render_error {
                                eprintln!("native publication after input: {error:?}");
                                owner_reports
                                    .notice(format!("Publication after input failed: {error:?}"));
                            }
                        })
                        .map_err(|error| {
                            *report.lock().expect("startup failure slot") =
                                Some((format!("{:?}", error.error), error.undelivered));
                            anyhow::anyhow!("native owner stopped")
                        })
                })
                .map_err(|error| (error.to_string(), vec![]))
            })();
            if let Err((error, mut rejected)) = result {
                if let Some(owner) = unbound.take() {
                    rejected.extend(owner.reject_unbound());
                }
                // Store failure before final publisher drop wakes the UI. No
                // polling, blocking UI receive, or infinite first-frame await.
                let mut slot = report.lock().expect("startup failure slot");
                if let Some((_, events)) = slot.as_mut() {
                    events.extend(rejected);
                } else {
                    *slot = Some((error, rejected));
                }
            }
            drop(keepalive);
        });
        cx.spawn(async move |cx| {
            let frame = loop {
                match updates.next().await {
                    Some(update) => {
                        if let Some(frame) = update.frame {
                            break frame;
                        }
                    }
                    None => {
                        let error = failure.lock().expect("startup failure slot").take();
                        eprintln!("native editor startup failed: {error:?}");
                        startup_failed.store(true, Ordering::Relaxed);
                        let _ = cx.update(|cx| cx.quit());
                        return;
                    }
                }
            };
            let _ = cx.update(move |cx| {
                let opened = cx.open_window(
                    WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::centered(
                            None,
                            size(px(bounds.width), px(bounds.height)),
                            cx,
                        ))),
                        ..Default::default()
                    },
                    move |window, cx| {
                        let rejected_reports = reports.clone();
                        let composition_reports = reports.clone();
                        let ime_reports = reports.clone();
                        let view = cx.new(|cx| {
                            FrameView::watch_updates(updates, cx).detach();
                            let mut view = FrameView::new_ime(
                                frame,
                                font,
                                sender,
                                replies,
                                platform_reader,
                                quit,
                                Rc::new(move |e| {
                                    let message = format!("Input not delivered: {:?}", e.reason);
                                    if let OwnerEvent::Evaluate { request_id, .. } = e.event {
                                        rejected_reports.evaluation(
                                            rune_gpui::editor_transport::EvaluationOutcome {
                                                request_id,
                                                outcome: Err(message),
                                            },
                                        );
                                    } else {
                                        rejected_reports.notice(message);
                                    }
                                }),
                                Rc::new(move |ack| {
                                    if let Err(error) = ack.outcome {
                                        composition_reports
                                            .notice(format!("Composition rejected: {error:?}"));
                                    }
                                }),
                                Rc::new(move |error| {
                                    ime_reports.notice(format!("IME input: {error:?}"));
                                }),
                                window,
                                cx,
                            )
                            .expect("prepared IME context");
                            view.set_evaluation_quit(evaluation_quit.clone())
                                .expect("editor owner view");
                            view
                        });
                        window.focus(&view.read(cx).focus_handle());
                        cx.new(|cx| {
                            NativeShell::new(
                                view,
                                reports,
                                report_updates,
                                evaluation_quit,
                                window,
                                cx,
                            )
                        })
                    },
                );
                if let Err(error) = opened {
                    eprintln!("native editor window creation failed: {error:?}");
                    startup_failed.store(true, Ordering::Relaxed);
                    cx.quit();
                    return;
                }
                cx.activate(true);
            });
        })
        .detach();
    });
    if failed.load(Ordering::Relaxed) { Err(()) } else { Ok(()) }
}
fn evaluate_source(host: &mut LispRuntimeHost<'_, '_, '_>, source: &str) -> Result<(), String> {
    host.evaluate_source(source)
}
fn evaluate_source_with_quit(
    host: &mut LispRuntimeHost<'_, '_, '_>,
    source: &str,
    token: Arc<AtomicBool>,
) -> Result<(), String> {
    host.evaluate_source_with_quit(source, token)
}

#[cfg(test)]
mod joined_owner_tests {
    use super::*;
    use rune_editor::Document;

    #[test]
    fn actual_persistent_lisp_host_runs_fifo_with_partial_error_and_native_undo() {
        with_lisp_runtime_owner(80, 24, |host| {
            let (publisher, updates) = host_updates();
            let (sender, _replies, pending) = pending_editor_transport(8, publisher);
            let bridge = EditorBridge::new(
                host,
                FixedMetrics::default(),
                Rect { x: 0., y: 0., width: 640., height: 480. },
            )
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let mut worker = pending
                .bind_initialized(bridge)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?
                .with_evaluator(evaluate_source);
            sender
                .try_send(OwnerEvent::Evaluate {
                    request_id: 1,
                    source: "(setq native-owner-value (list \"λ🙂\"))".into(),
                })
                .unwrap();
            sender
                .try_send(OwnerEvent::Evaluate {
                    request_id: 2,
                    source: "(insert (car native-owner-value))".into(),
                })
                .unwrap();
            for expected in [1, 2] {
                let step = worker.try_step().unwrap();
                let evaluation = step.evaluation.unwrap();
                assert_eq!(evaluation.request_id, expected);
                assert!(evaluation.outcome.is_ok());
                assert!(step.edit.is_ok());
            }
            let snapshot = worker.bridge().snapshot().unwrap();
            let selected = snapshot
                .views
                .iter()
                .find(|v| v.id == snapshot.selected_window)
                .unwrap()
                .view
                .buffer;
            assert_eq!(
                snapshot.buffers.iter().find(|b| b.id == selected).unwrap().document.to_text(),
                "λ🙂"
            );
            drop(snapshot);

            // A signal can follow an applied edit. A later frame failure must
            // neither roll that edit back nor replay the evaluation callback.
            drop(updates);
            sender
                .try_send(OwnerEvent::Evaluate {
                    request_id: 3,
                    source: "(insert \"x\") (error \"signal after edit\")".into(),
                })
                .unwrap();
            let step = worker.try_step().unwrap();
            assert!(step.edit.unwrap());
            assert!(step.evaluation.unwrap().outcome.is_err());
            assert!(step.render_error.is_some());
            assert!(worker.retry_frame().is_err());
            let snapshot = worker.bridge().snapshot().unwrap();
            assert_eq!(
                snapshot.buffers.iter().find(|b| b.id == selected).unwrap().document.to_text(),
                "λ🙂x"
            );
            drop(snapshot);
            sender.try_send(OwnerEvent::Command(rune_editor::Command::Undo)).unwrap();
            assert!(worker.try_step().unwrap().edit.unwrap());
            let snapshot = worker.bridge().snapshot().unwrap();
            assert_eq!(
                snapshot.buffers.iter().find(|b| b.id == selected).unwrap().document.to_text(),
                "λ🙂"
            );
            Ok(())
        })
        .unwrap();
    }
    thread_local! {
        static ACTIVE_QUIT: std::cell::RefCell<Option<EvaluationQuit>> = const { std::cell::RefCell::new(None) };
    }
    fn signal_active_evaluation(
        host: &mut LispRuntimeHost<'_, '_, '_>,
        source: &str,
        token: Arc<AtomicBool>,
    ) -> Result<(), String> {
        assert!(!token.load(Ordering::Acquire));
        if source.contains("request-native-quit") {
            ACTIVE_QUIT.with(|slot| assert!(slot.borrow().as_ref().unwrap().request()));
            assert!(token.load(Ordering::Acquire));
        }
        evaluate_source_with_quit(host, source, token)
    }
    #[test]
    fn actual_scoped_quit_preserves_cleanup_partial_edit_and_next_fifo_request() {
        with_lisp_runtime_owner(80, 24, |host| {
            let signal = EvaluationQuit::default();
            assert!(!signal.request());
            ACTIVE_QUIT.with(|slot| *slot.borrow_mut() = Some(signal.clone()));
            let (publisher, updates) = host_updates();
            let (sender, _replies, pending) = pending_editor_transport(8, publisher);
            let bridge = EditorBridge::new(host, FixedMetrics::default(), Rect {x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
            let (platform, _reader) = platform_frames();
            let mut worker = pending.bind_ime_initialized(bridge, platform).map_err(|e|anyhow::anyhow!("{e:?}"))?.with_interruptible_evaluator(signal_active_evaluation, signal.clone());
            sender.try_send(OwnerEvent::Evaluate {request_id:1, source:"(setq inhibit-quit t)".into()}).unwrap();
            let first = worker.try_step().unwrap();
            assert!(first.evaluation.unwrap().outcome.is_ok());
            assert!(!signal.request());
            drop(updates);
            sender.try_send(OwnerEvent::Evaluate {request_id:2, source:"; request-native-quit\n(unwind-protect (progn (insert \"x\") (setq inhibit-quit nil) (+ 1 2)) (insert \"c\"))".into()}).unwrap();
            let quit = worker.try_step().unwrap();
            assert!(quit.edit.unwrap());
            let outcome = quit.evaluation.unwrap();
            assert_eq!(outcome.request_id, 2);
            assert!(outcome.outcome.unwrap_err().to_lowercase().contains("quit"));
            assert!(quit.render_error.is_some());
            assert!(!signal.request());
            assert!(worker.retry_frame().is_err());
            assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "xc");
            sender.try_send(OwnerEvent::Evaluate {request_id:3, source:"(insert \"n\")".into()}).unwrap();
            let next = worker.try_step().unwrap();
            assert!(next.edit.unwrap());
            assert!(next.evaluation.unwrap().outcome.is_ok());
            assert!(!signal.request());
            assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "xcn");
            sender.try_send(OwnerEvent::Command(rune_editor::Command::Undo)).unwrap();
            assert!(worker.try_step().unwrap().edit.unwrap());
            assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "xc");
            ACTIVE_QUIT.with(|slot| *slot.borrow_mut() = None);
            Ok(())
        }).unwrap();
    }
    #[test]
    fn startup_source_caps_reject_before_application_or_window_creation() {
        assert!(run(vec![String::new(); 65]).is_err());
        assert!(run(vec!["x".repeat(65_537)]).is_err());
    }
}
