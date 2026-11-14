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
pub(crate) fn run(sources: Vec<String>, native_file: Option<std::path::PathBuf>) -> Result<(), ()> {
    if sources.len() > 64 || sources.iter().any(|s| s.len() > 65_536) {
        eprintln!("native startup permits at most 64 sources of at most 65536 bytes each");
        return Err(());
    }
    let guarded_file_mode = native_file.is_some();
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
                with_lisp_runtime_owner(80, 24, |mut host| {
                    prepare_explicit_file(&mut host, native_file.as_deref())
                        .map_err(anyhow::Error::msg)?;
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
                    configure_native_saver(
                        worker
                            .with_key_dispatcher(dispatch_native_key, owner_quit.clone())
                            .with_interruptible_evaluator(evaluate_source_with_quit, owner_quit),
                        guarded_file_mode,
                    )
                    .run_with_outcome(|step| {
                        if let Some(evaluation) = step.evaluation {
                            if let Err(error) = &evaluation.outcome {
                                eprintln!("native evaluation {}: {error}", evaluation.request_id);
                            }
                            owner_reports.evaluation(evaluation);
                        }
                        if let Some(save) = step.save {
                            owner_reports.save(save);
                        }
                        if let Some(Err(error)) = step.key_outcome {
                            owner_reports.notice(format!("Key command failed: {error}"));
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
                                    match e.event {
                                        OwnerEvent::Evaluate { request_id, .. } => rejected_reports
                                            .evaluation(
                                                rune_gpui::editor_transport::EvaluationOutcome {
                                                    request_id,
                                                    outcome: Err(message),
                                                },
                                            ),
                                        OwnerEvent::SaveVisited { request_id, .. } => {
                                            rejected_reports.save(
                                                rune_gpui::editor_transport::SaveReply {
                                                    request_id,
                                                    outcome: Err(message),
                                                },
                                            )
                                        }
                                        _ => rejected_reports.notice(message),
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
                                guarded_file_mode,
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
const MAX_NATIVE_FILE_BYTES: usize = 16 * 1024 * 1024;
fn prepare_explicit_file(
    host: &mut LispRuntimeHost<'_, '_, '_>,
    path: Option<&std::path::Path>,
) -> Result<(), String> {
    if let Some(path) = path {
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "visited".into());
        let label = format!(
            "file: {}",
            filename
                .chars()
                .map(|c| if c.is_control() { '�' } else { c })
                .take(255)
                .collect::<String>()
        );
        host.open_selected_trusted_file(&label, path, MAX_NATIVE_FILE_BYTES)?;
    }
    Ok(())
}
fn configure_native_saver<'a, 'roots, 'env>(
    worker: rune_gpui::editor_transport::NativeEditorWorker<LispRuntimeHost<'a, 'roots, 'env>>,
    guarded: bool,
) -> rune_gpui::editor_transport::NativeEditorWorker<LispRuntimeHost<'a, 'roots, 'env>> {
    if guarded {
        worker.with_visited_saver(save_selected_guarded_visited)
    } else {
        worker.with_visited_saver(save_selected_visited)
    }
}
fn save_selected_guarded_visited(
    host: &mut LispRuntimeHost<'_, '_, '_>,
    max_bytes: usize,
) -> Result<rune_gpui::editor_transport::VisitedSave, String> {
    let saved = host.save_selected_guarded_visited(max_bytes)?;
    Ok(rune_gpui::editor_transport::VisitedSave {
        kind: match saved.kind {
            crate::editor_document::SaveKind::Saved => {
                rune_gpui::editor_transport::VisitedSaveKind::Saved
            }
            crate::editor_document::SaveKind::Unchanged => {
                rune_gpui::editor_transport::VisitedSaveKind::Unchanged
            }
        },
        path: saved.path,
        bytes: saved.bytes,
    })
}

fn evaluate_source(host: &mut LispRuntimeHost<'_, '_, '_>, source: &str) -> Result<(), String> {
    host.evaluate_source(source)
}
fn save_selected_visited(
    host: &mut LispRuntimeHost<'_, '_, '_>,
    max_bytes: usize,
) -> Result<rune_gpui::editor_transport::VisitedSave, String> {
    let saved = host.save_selected_visited(max_bytes)?;
    Ok(rune_gpui::editor_transport::VisitedSave {
        kind: match saved.kind {
            crate::editor_document::SaveKind::Saved => {
                rune_gpui::editor_transport::VisitedSaveKind::Saved
            }
            crate::editor_document::SaveKind::Unchanged => {
                rune_gpui::editor_transport::VisitedSaveKind::Unchanged
            }
        },
        path: saved.path,
        bytes: saved.bytes,
    })
}

fn dispatch_native_key(
    host: &mut LispRuntimeHost<'_, '_, '_>,
    request: &rune_gpui::editor_transport::NativeKeyRequest,
    token: Arc<AtomicBool>,
) -> Result<bool, String> {
    host.dispatch_native_key_with_quit(
        request.window_id,
        request.buffer_id,
        &request.key,
        request.control,
        request.meta,
        request.shift,
        token,
    )
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
    use rune_editor::{Document, EditorHost};

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
        assert!(run(vec![String::new(); 65], None).is_err());
        assert!(run(vec!["x".repeat(65_537)], None).is_err());
    }
    struct OwnedStartupDirectory(std::path::PathBuf);
    impl OwnedStartupDirectory {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("rune-native-open-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for OwnedStartupDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn explicit_open_failure_rejects_original_queued_requests_once_before_bind() {
        use std::{cell::Cell, sync::mpsc::TrySendError};
        let dir = OwnedStartupDirectory::new();
        let missing = dir.0.join("missing");
        let (publisher, mut updates) = host_updates();
        let keepalive = publisher.clone();
        let (sender, mut replies, pending) = pending_editor_transport(4, publisher);
        for (id, source) in [(101, "(insert \"not-run λ🙂\")"), (102, "(setq never-run t)")] {
            sender
                .try_send(OwnerEvent::Evaluate { request_id: id, source: source.into() })
                .unwrap();
        }
        let mut unbound = Some(pending);
        let bound = Cell::new(false);
        let result = with_lisp_runtime_owner(80, 24, |mut host| {
            prepare_explicit_file(&mut host, Some(&missing)).map_err(anyhow::Error::msg)?;
            bound.set(true);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!bound.get());
        let rejected = unbound.take().unwrap().reject_unbound();
        assert_eq!(rejected.len(), 2);
        assert!(
            matches!(&rejected[0],OwnerEvent::Evaluate{request_id:101,source}if source=="(insert \"not-run λ🙂\")")
        );
        assert!(
            matches!(&rejected[1],OwnerEvent::Evaluate{request_id:102,source}if source=="(setq never-run t)")
        );
        assert!(
            matches!(sender.try_send(OwnerEvent::Evaluate{request_id:103,source:"late".into()}),Err(TrySendError::Disconnected(OwnerEvent::Evaluate{request_id:103,source}))if source=="late")
        );
        assert!(replies.take().is_none());
        // A failure record exists before the last publication handle closes.
        let failure_record = (result.unwrap_err().to_string(), rejected);
        assert!(!failure_record.0.is_empty());
        drop(keepalive);
        use std::{
            future::Future,
            task::{Context as TaskContext, Poll, Waker},
        };
        let mut next = std::pin::pin!(updates.next());
        let mut cx = TaskContext::from_waker(Waker::noop());
        assert!(matches!(next.as_mut().poll(&mut cx), Poll::Ready(None)));
    }
    #[test]
    fn explicit_file_policy_uses_guarded_native_save_and_does_not_force_conflict() {
        for guarded in [true, false] {
            let dir = OwnedStartupDirectory::new();
            let path = dir.0.join("file.txt");
            std::fs::write(&path, "original").unwrap();
            with_lisp_runtime_owner(80, 24, |mut host| {
                prepare_explicit_file(&mut host, Some(&path)).map_err(anyhow::Error::msg)?;
                let (publisher, _updates) = host_updates();
                let (sender, _replies, pending) = pending_editor_transport(4, publisher);
                let bridge = EditorBridge::new(
                    host,
                    FixedMetrics::default(),
                    Rect { x: 0., y: 0., width: 640., height: 480. },
                )
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                let worker = pending
                    .bind_initialized(bridge)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?
                    .with_evaluator(evaluate_source);
                let mut worker = configure_native_saver(worker, guarded);
                sender
                    .try_send(OwnerEvent::Evaluate {
                        request_id: 1,
                        source: "(erase-buffer) (insert \"edited\")".into(),
                    })
                    .unwrap();
                assert!(worker.try_step().unwrap().evaluation.unwrap().outcome.is_ok());
                std::fs::write(&path, "external")?;
                sender
                    .try_send(OwnerEvent::SaveVisited { request_id: 2, max_bytes: 64 })
                    .unwrap();
                let reply = worker.try_step().unwrap().save.unwrap();
                if guarded {
                    assert!(reply.outcome.is_err());
                    assert_eq!(std::fs::read_to_string(&path)?, "external");
                    std::fs::remove_file(&path)?;
                    sender
                        .try_send(OwnerEvent::SaveVisited { request_id: 3, max_bytes: 64 })
                        .unwrap();
                    assert!(worker.try_step().unwrap().save.unwrap().outcome.is_err());
                    assert!(!path.exists());
                } else {
                    assert_eq!(
                        reply.outcome.unwrap().kind,
                        rune_gpui::editor_transport::VisitedSaveKind::Saved
                    );
                    assert_eq!(std::fs::read_to_string(&path)?, "edited");
                }
                Ok(())
            })
            .unwrap();
        }
    }
    #[test]
    fn long_org_scoped_wheel_moves_viewport_without_point_or_text_and_rejects_stale() {
        use std::{
            future::Future,
            task::{Context as TaskContext, Poll, Waker},
        };
        let dir = OwnedStartupDirectory::new();
        let path = dir.0.join("long.org");
        let source = (0..2000)
            .map(|n| format!("* TODO Item {n} λ🙂\n  body {n}\n"))
            .collect::<String>();
        std::fs::write(&path, &source).unwrap();
        with_lisp_runtime_owner(80, 24, |mut host| {
            prepare_explicit_file(&mut host, Some(&path)).map_err(anyhow::Error::msg)?;
            let (publisher, mut updates) = host_updates();
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
            let before = worker.bridge().snapshot().unwrap();
            let view = before.views.iter().find(|v| v.id == before.selected_window).unwrap();
            let (window_id, buffer_id, point, start, sequence) = (
                view.id.as_u64(),
                view.view.buffer.as_u64(),
                view.view.point,
                view.view.start,
                before.sequence,
            );
            sender
                .try_send(OwnerEvent::ScrollWindow {
                    window_id,
                    buffer_id,
                    expected_sequence: sequence,
                    lines: 100,
                })
                .unwrap();
            assert_eq!(worker.try_step().unwrap().edit.unwrap(), true);
            let after = worker.bridge().snapshot().unwrap();
            let v = after.views.iter().find(|v| v.id == after.selected_window).unwrap();
            assert!(v.view.start > start);
            assert_eq!(v.view.point, point);
            assert_eq!(
                after.buffers.iter().find(|b| b.id == v.view.buffer).unwrap().document.to_text(),
                source
            );
            let update = {
                let mut next = std::pin::pin!(updates.next());
                let mut cx = TaskContext::from_waker(Waker::noop());
                match next.as_mut().poll(&mut cx) {
                    Poll::Ready(Some(u)) => u,
                    _ => panic!("published scroll frame missing"),
                }
            };
            let frame = update.frame.unwrap();
            let painted = frame.windows.iter().find(|w| w.window_id == window_id).unwrap();
            assert!(
                painted.cursor.is_none(),
                "manual scrolling must not snap back to offscreen point"
            );
            assert!(painted.rows.first().unwrap().source.start >= v.view.start);
            assert!(
                painted.scanned_chars < source.chars().count() / 4,
                "layout stays viewport bounded"
            );
            sender
                .try_send(OwnerEvent::Evaluate {
                    request_id: 99,
                    source: "(put-text-property 1 2 'face 'bold)".into(),
                })
                .unwrap();
            assert!(worker.try_step().unwrap().evaluation.unwrap().outcome.is_ok());
            let property_snapshot = worker.bridge().snapshot().unwrap();
            assert!(property_snapshot.sequence > after.sequence);
            let update = {
                let mut next = std::pin::pin!(updates.next());
                let mut cx = TaskContext::from_waker(Waker::noop());
                match next.as_mut().poll(&mut cx) {
                    Poll::Ready(Some(u)) => u,
                    _ => panic!("property frame missing"),
                }
            };
            let property_frame = update.frame.unwrap();
            assert_eq!(property_frame.windows[0].rows[0].source, painted.rows[0].source);
            assert!(property_frame.windows[0].cursor.is_none());
            let after = property_snapshot;
            sender
                .try_send(OwnerEvent::ScrollWindow {
                    window_id,
                    buffer_id,
                    expected_sequence: sequence,
                    lines: 100,
                })
                .unwrap();
            assert!(worker.try_step().unwrap().edit.is_err());
            assert_eq!(worker.bridge().snapshot().unwrap().sequence, after.sequence);
            sender
                .try_send(OwnerEvent::ScrollWindow {
                    window_id,
                    buffer_id,
                    expected_sequence: after.sequence,
                    lines: -100,
                })
                .unwrap();
            assert!(worker.try_step().unwrap().edit.unwrap());
            assert_eq!(
                worker
                    .bridge()
                    .snapshot()
                    .unwrap()
                    .views
                    .iter()
                    .find(|v| v.id == after.selected_window)
                    .unwrap()
                    .view
                    .start,
                start
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn real_keymap_fifo_intercepts_printable_once_but_paste_and_ime_stay_literal() {
        with_lisp_runtime_owner(80,24,|mut host| {
            host.evaluate_source("(setq keymap-hits 0) (fset 'keymap-hit '(lambda () (interactive) (setq keymap-hits (+ keymap-hits 1)) (insert \"B\"))) (fset 'keymap-bad '(lambda () (interactive) (insert \"BAD\"))) (setq owner-keymap (make-sparse-keymap)) (define-key owner-keymap \"xy\" 'keymap-hit) (define-key owner-keymap [955] 'keymap-bad) (use-global-map owner-keymap)").map_err(anyhow::Error::msg)?;
            let (publisher,_updates)=host_updates();let(sender,_replies,pending)=pending_editor_transport(16,publisher);
            let(platform,reader)=rune_gpui::platform_context::platform_frames();
            let bridge=EditorBridge::new(host,FixedMetrics::default(),Rect{x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
            let quit=rune_gpui::evaluation_quit::EvaluationQuit::default();
            let mut worker=pending.bind_ime_initialized(bridge,platform).map_err(|e|anyhow::anyhow!("{e:?}"))?.with_key_dispatcher(dispatch_native_key,quit).with_evaluator(evaluate_source);
            let initial=worker.bridge().snapshot().unwrap();let v=initial.views.iter().find(|v|v.id==initial.selected_window).unwrap();let(window_id,buffer_id)=(v.id.as_u64(),v.view.buffer.as_u64());
            sender.try_send(OwnerEvent::KeyText{window_id,buffer_id,text:"x".into()}).unwrap();
            let step=worker.try_step().unwrap();assert!(step.key_outcome.unwrap().is_ok());assert!(!step.edit.unwrap());
            sender.try_send(OwnerEvent::Evaluate{request_id:1,source:"(garbage-collect)".into()}).unwrap();assert!(worker.try_step().unwrap().evaluation.unwrap().outcome.is_ok());
            sender.try_send(OwnerEvent::KeyText{window_id,buffer_id,text:"y".into()}).unwrap();assert!(worker.try_step().unwrap().key_outcome.unwrap().is_ok());
            sender.try_send(OwnerEvent::TextInsertion{window_id,buffer_id,text:"xy".into()}).unwrap();assert!(worker.try_step().unwrap().edit.unwrap());
            sender.try_send(OwnerEvent::KeyText{window_id,buffer_id,text:"z".into()}).unwrap();assert!(worker.try_step().unwrap().edit.unwrap());
            let mut ime=rune_gpui::platform_state::PlatformInputState::new(reader,FixedMetrics::default()).unwrap();ime.sync();
            ime.replace_and_mark(None,"λ",None).unwrap();let event=ime.commit(None,"λ").unwrap();assert!(matches!(&event,OwnerEvent::Composition(_)));
            sender.try_send(event).unwrap();assert!(worker.try_step().unwrap().edit.unwrap());
            let final_snapshot=worker.bridge().snapshot().unwrap();let buffer=final_snapshot.buffers.iter().find(|b|b.id.as_u64()==buffer_id).unwrap();assert_eq!(buffer.document.to_text(),"Bxyzλ");
            sender.try_send(OwnerEvent::Evaluate{request_id:2,source:"(if (= keymap-hits 1) nil (error \"duplicate key invocation\"))".into()}).unwrap();assert!(worker.try_step().unwrap().evaluation.unwrap().outcome.is_ok());
            Ok(())
        }).unwrap();
    }
}
