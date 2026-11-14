//! Real headless owner/FIFO/plain mailbox/native admission, not Window paint.
use super::*;
use rune_display::{MeasuredFontCapabilities, MeasuredFontVariant as V};
use rune_editor::{Document, EditorHost};
use rune_gpui::font_transport::{FontRunProjection, font_plan_mailbox};
use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    task::{Context as TaskContext, Poll, Waker},
};
fn take_frame(updates: &mut rune_display::HostUpdates) -> Arc<rune_display::FrameDisplay> {
    let mut next = updates.next();
    let mut cx = TaskContext::from_waker(Waker::noop());
    match Pin::new(&mut next).poll(&mut cx) {
        Poll::Ready(Some(update)) => update.frame.unwrap(),
        _ => panic!("no published frame"),
    }
}
#[test]
fn actual_headless_owner_font_fifo_reprojects_partial_errors_without_replay() {
    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::headless().run(move|cx|{
        let test=(||->anyhow::Result<(gpui::Entity<FrameView>,gpui::Task<()>,Arc<rune_display::FrameFontPlan>,Rc<rune_gpui::NativeFontVariantBank>)>{
            let base=NativeFont::measure(gpui::font("DejaVu Sans Mono"),17.,3.,cx).map_err(anyhow::Error::msg)?;
            let bank=Rc::new(rune_gpui::NativeFontVariantBank::measure(&base,cx).map_err(anyhow::Error::msg)?);
            let cap=bank.capabilities(cx).map_err(anyhow::Error::msg)?;
            anyhow::ensure!(cap.bold,"diagnostic needs a real measured bold face");
            let (font_publisher,font_receiver)=font_plan_mailbox();
            let (publisher,mut updates)=host_updates();let wake_publisher=publisher.clone();let (sender,replies,pending)=pending_editor_transport(8,publisher);
            // Single UI producer queues requests before the owner constructs its host.
            sender.try_send(OwnerEvent::Evaluate{request_id:8101,source:"(aset (internal-lisp-face-p 'pipeline-face (selected-frame)) 5 'bold) (error \"after font edit\")".into()}).unwrap();
            sender.try_send(OwnerEvent::Evaluate{request_id:8102,source:"(setq font-pipeline-count (1+ font-pipeline-count)) (put-text-property 1 3 'face '(:weight semibold))".into()}).unwrap();
            sender.try_send(OwnerEvent::Evaluate{request_id:8103,source:"(if (/= font-pipeline-count 1) (error \"replayed\")) (put-text-property 1 3 'face 'pipeline-face) (garbage-collect)".into()}).unwrap();
            let metrics=FixedMetrics(cap.metrics);
            let owner=std::thread::spawn(move||with_lisp_runtime_owner(80,24,|mut host|{
                host.evaluate_source("(insert \"ab\") (setq font-pipeline-count 0) (internal-make-lisp-face 'default (selected-frame)) (internal-make-lisp-face 'pipeline-face (selected-frame)) (put-text-property 1 3 'face 'pipeline-face)").map_err(anyhow::Error::msg)?;
                let bridge=EditorBridge::new(host,metrics,Rect{x:0.,y:0.,width:metrics.0.advance*80.,height:metrics.0.line_height()*24.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
                let mut worker=pending.bind_font_initialized(bridge,cap,crate::editor_document::native_font_projection,font_publisher).map_err(|e|anyhow::anyhow!("{e:?}"))?.with_evaluator(evaluate_source);
                let initial=worker.bridge().prepared_font_plan().unwrap();let sequence=worker.bridge().snapshot().map_err(|e|anyhow::anyhow!("{e:?}"))?.sequence;
                let partial=worker.try_step().unwrap();assert_eq!(partial.evaluation.as_ref().unwrap().request_id,8101);assert!(partial.evaluation.unwrap().outcome.is_err());assert!(partial.edit.unwrap());assert!(partial.render_error.is_none());assert_eq!(worker.bridge().snapshot().map_err(|e|anyhow::anyhow!("{e:?}"))?.sequence,sequence);
                let bold=worker.bridge().prepared_font_plan().unwrap();assert!(bold.frame().generation>initial.frame().generation);assert!(bold.assignments().iter().any(|a|a.variant==V::Bold));
                let failed=worker.try_step().unwrap();assert!(failed.evaluation.unwrap().outcome.is_ok());assert!(failed.render_error.is_some());assert!(Arc::ptr_eq(&bold,&worker.bridge().prepared_font_plan().unwrap()));assert!(worker.retry_frame().is_err());
                let repaired=worker.try_step().unwrap();assert!(repaired.evaluation.unwrap().outcome.is_ok());assert!(repaired.render_error.is_none());
                let final_plan=worker.bridge().prepared_font_plan().unwrap();let snapshot=worker.bridge().snapshot().map_err(|e|anyhow::anyhow!("{e:?}"))?;let view=snapshot.views.iter().find(|v|v.id==snapshot.selected_window).unwrap();assert_eq!(snapshot.buffers.iter().find(|b|b.id==view.view.buffer).unwrap().document.to_text(),"ab");
                Ok((initial,bold,final_plan))
            }));
            let (initial,bold,final_plan)=owner.join().map_err(|_|anyhow::anyhow!("owner panicked"))??;
            let published=font_receiver.latest().map_err(anyhow::Error::msg)?.unwrap();assert!(Arc::ptr_eq(&published,&final_plan));assert!(published.matches_frame(&take_frame(&mut updates)));
            for plan in [initial.clone(),bold.clone(),published.clone()]{let native=rune_gpui::NativeFrameFontPlan::new((*plan).clone(),&base,bank.clone(),cx).map_err(anyhow::Error::msg)?;for assignment in plan.assignments(){assert_eq!(native.font_for_face(assignment.face_id,cx).map_err(anyhow::Error::msg)?,bank.get_compatible_font(cap.bank_id,cap.metrics,assignment.variant,cx).map_err(anyhow::Error::msg)?);}}
            println!("HEADLESS_OWNER_FONT_FIFO_GATE_PASS exact_arc_pair=true direct_mutation_reprojected=true partial_error_preserved=true callback_not_replayed=true");
            let view=cx.new(|cx|FrameView::new_font_editor(initial.clone(),bank.clone(),base.clone(),sender,replies,Arc::new(AtomicBool::new(false)),Rc::new(|_|{}),Rc::new(|_|{}),cx).expect("matching initial font plan"));
            view.update(cx,|view,cx|{
                assert!(!view.present_measured_font_plan(initial.clone(),bank.clone(),cx).unwrap());
                assert!(view.present(initial.frame().clone(),cx).is_err());
                let wrong=rune_display::FrameFontPlan::new(bold.frame().clone(),cap.bank_id.checked_add(1).unwrap(),cap.metrics,bold.assignments().to_vec()).unwrap();
                assert!(view.present_measured_font_plan(Arc::new(wrong),bank.clone(),cx).is_err());
                assert!(view.present_measured_font_plan(bold.clone(),bank.clone(),cx).unwrap());
                assert!(!view.present_measured_font_plan(bold.clone(),bank.clone(),cx).unwrap());
                assert!(!view.present_measured_font_plan(initial.clone(),bank.clone(),cx).unwrap());
                assert!(view.present(published.frame().clone(),cx).is_err());
            });
            // A capacity-only wake must still consume the latest paired frame.
            wake_publisher.input_capacity_available().map_err(anyhow::Error::msg)?;drop(wake_publisher);
            let task=view.update(cx,|_,cx|FrameView::watch_font_updates(updates,font_receiver,bank.clone(),cx));
            Ok((view,task,published,bank))
        })();
        match test{
            Ok((view,task,final_plan,bank))=>{
                cx.spawn(async move|cx|{
                    task.await;
                    // Keep the entity alive through the watcher, then verify it
                    // advanced to the final generation and closed disconnected input.
                    let checked=view.update(cx,|view,cx|{
                        let current=view.present_measured_font_plan(final_plan,bank,cx);
                        (current,view.input_readiness())
                    });
                    match checked{
                        Ok((Ok(false),rune_gpui::InputReadiness::Disconnected))=>{
                            result.set(true);
                            println!("HEADLESS_FRAMEVIEW_ENTITY_WATCHER_GATE_PASS constructor=true generation_and_bank_guards=true capacity_wake_admitted_pair=true input_closed=true");
                        },
                        _=>eprintln!("HEADLESS_FRAMEVIEW_ENTITY_WATCHER_GATE_ERROR final generation or lifecycle mismatch"),
                    }
                    let _=cx.update(|app|rune_gpui::quit_on_next_foreground_turn(app));
                }).detach();
            },
            Err(e)=>{eprintln!("HEADLESS_OWNER_FONT_FIFO_GATE_ERROR {e:#}");rune_gpui::quit_on_next_foreground_turn(cx);}
        }
    });
    assert!(passed.get(), "owner/font pipeline failed");
}
fn bad_epoch(
    host: &LispRuntimeHost<'_, '_, '_>,
    w: rune_editor::WindowId,
    b: rune_editor::BufferId,
    len: usize,
    r: std::ops::Range<usize>,
    c: MeasuredFontCapabilities,
) -> Result<FontRunProjection, String> {
    let mut p = crate::editor_document::native_font_projection(host, w, b, len, r, c)?;
    p.publication_epoch += 1;
    Ok(p)
}
fn bad_sequence(
    host: &LispRuntimeHost<'_, '_, '_>,
    w: rune_editor::WindowId,
    b: rune_editor::BufferId,
    len: usize,
    r: std::ops::Range<usize>,
    c: MeasuredFontCapabilities,
) -> Result<FontRunProjection, String> {
    let mut p = crate::editor_document::native_font_projection(host, w, b, len, r, c)?;
    p.owner_sequence += 1;
    Ok(p)
}
fn bad_frame(
    host: &LispRuntimeHost<'_, '_, '_>,
    w: rune_editor::WindowId,
    b: rune_editor::BufferId,
    len: usize,
    r: std::ops::Range<usize>,
    c: MeasuredFontCapabilities,
) -> Result<FontRunProjection, String> {
    let mut p = crate::editor_document::native_font_projection(host, w, b, len, r, c)?;
    p.frame_id += 1;
    Ok(p)
}
fn failed_projection(
    _host: &LispRuntimeHost<'_, '_, '_>,
    _w: rune_editor::WindowId,
    _b: rune_editor::BufferId,
    _len: usize,
    _r: std::ops::Range<usize>,
    _c: MeasuredFontCapabilities,
) -> Result<FontRunProjection, String> {
    Err("injected font callback failure".into())
}
#[test]
fn font_pipeline_stale_scope_and_callback_failure_reject_unprocessed_fifo() {
    for mode in 0..5 {
        with_lisp_runtime_owner(80,24,|host|{
        let(publisher,_updates)=host_updates();let(sender,mut replies,pending)=pending_editor_transport(4,publisher);let(font_publisher,font_receiver)=font_plan_mailbox();let font_receiver=if mode==4{drop(font_receiver);None}else{Some(font_receiver)};
        sender.try_send(OwnerEvent::Evaluate{request_id:8201,source:"(insert \"unprocessed\")".into()}).unwrap();
        let cap=MeasuredFontCapabilities{bank_id:7,metrics:FixedMetrics::default().0,normal:true,bold:true,italic:true,bold_italic:true};

        let bridge=EditorBridge::new(host,FixedMetrics(cap.metrics),Rect{x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
        let initialized=match mode{0=>pending.bind_font_initialized(bridge,cap,bad_epoch,font_publisher),1=>pending.bind_font_initialized(bridge,cap,bad_sequence,font_publisher),2=>pending.bind_font_initialized(bridge,cap,bad_frame,font_publisher),3=>pending.bind_font_initialized(bridge,cap,failed_projection,font_publisher),_=>pending.bind_font_initialized(bridge,cap,crate::editor_document::native_font_projection,font_publisher)};
        let failure=match initialized{Ok(_)=>panic!("invalid projection initialized"),Err(e)=>e};
        assert_eq!(failure.undelivered.len(),1);assert!(matches!(&failure.undelivered[0],OwnerEvent::Evaluate{request_id:8201,source} if source=="(insert \"unprocessed\")"));if let Some(receiver)=font_receiver {assert!(receiver.latest().unwrap().is_none());}assert!(replies.take().is_none());
        assert!(matches!(sender.try_send(OwnerEvent::Evaluate{request_id:8202,source:"late".into()}),Err(std::sync::mpsc::TrySendError::Disconnected(OwnerEvent::Evaluate{request_id:8202,..}))));Ok(())
    }).unwrap();
    }
}

#[test]
fn dropped_font_receiver_keeps_committed_edit_and_never_replays_on_retry() {
    with_lisp_runtime_owner(80, 24, |mut host| {
        host.evaluate_source("(insert \"x\")")
            .map_err(anyhow::Error::msg)?;
        let (publisher, _updates) = host_updates();
        let (sender, _replies, pending) = pending_editor_transport(4, publisher);
        let (font_publisher, font_receiver) = font_plan_mailbox();
        let cap = MeasuredFontCapabilities {
            bank_id: 8,
            metrics: FixedMetrics::default().0,
            normal: true,
            bold: true,
            italic: true,
            bold_italic: true,
        };
        let bridge = EditorBridge::new(
            host,
            FixedMetrics(cap.metrics),
            Rect {
                x: 0.,
                y: 0.,
                width: 640.,
                height: 480.,
            },
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let mut worker = pending
            .bind_font_initialized(
                bridge,
                cap,
                crate::editor_document::native_font_projection,
                font_publisher,
            )
            .map_err(|e| anyhow::anyhow!("{e:?}"))?
            .with_evaluator(evaluate_source);
        drop(font_receiver);
        sender
            .try_send(OwnerEvent::Evaluate {
                request_id: 8301,
                source: "(insert \"k\")".into(),
            })
            .unwrap();
        let step = worker.try_step().unwrap();
        assert!(step.evaluation.unwrap().outcome.is_ok());
        assert!(step.edit.unwrap());
        assert!(step.render_error.is_some());
        for _ in 0..2 {
            assert!(worker.retry_frame().is_err());
        }
        let s = worker
            .bridge()
            .snapshot()
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let v = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        assert_eq!(
            s.buffers
                .iter()
                .find(|b| b.id == v.view.buffer)
                .unwrap()
                .document
                .to_text(),
            "xk"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn font_pair_publish_before_ordinary_failure_is_visible_without_command_replay() {
    // A paired envelope is atomic internally, not atomic with the ordinary wake.
    with_lisp_runtime_owner(80, 24, |host| {
        let (publisher, updates) = host_updates();
        let (sender, _replies, pending) = pending_editor_transport(4, publisher);
        let (font_publisher, font_receiver) = font_plan_mailbox();
        sender
            .try_send(OwnerEvent::Evaluate {
                request_id: 8401,
                source: "(insert \"unprocessed\")".into(),
            })
            .unwrap();
        drop(updates);
        let cap = MeasuredFontCapabilities {
            bank_id: 9,
            metrics: FixedMetrics::default().0,
            normal: true,
            bold: true,
            italic: true,
            bold_italic: true,
        };
        let bridge = EditorBridge::new(
            host,
            FixedMetrics(cap.metrics),
            Rect {
                x: 0.,
                y: 0.,
                width: 640.,
                height: 480.,
            },
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let failure = match pending.bind_font_initialized(
            bridge,
            cap,
            crate::editor_document::native_font_projection,
            font_publisher,
        ) {
            Ok(_) => panic!("closed ordinary receiver initialized"),
            Err(e) => e,
        };
        assert_eq!(failure.undelivered.len(), 1);
        assert!(matches!(
            &failure.undelivered[0],
            OwnerEvent::Evaluate {
                request_id: 8401,
                ..
            }
        ));
        assert!(
            font_receiver.latest().unwrap().is_some(),
            "paired publish precedes ordinary startup failure"
        );
        Ok(())
    })
    .unwrap();
    with_lisp_runtime_owner(80, 24, |host| {
        let (publisher, updates) = host_updates();
        let (sender, _replies, pending) = pending_editor_transport(4, publisher);
        let (font_publisher, font_receiver) = font_plan_mailbox();
        let cap = MeasuredFontCapabilities {
            bank_id: 10,
            metrics: FixedMetrics::default().0,
            normal: true,
            bold: true,
            italic: true,
            bold_italic: true,
        };
        let bridge = EditorBridge::new(
            host,
            FixedMetrics(cap.metrics),
            Rect {
                x: 0.,
                y: 0.,
                width: 640.,
                height: 480.,
            },
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let mut worker = pending
            .bind_font_initialized(
                bridge,
                cap,
                crate::editor_document::native_font_projection,
                font_publisher,
            )
            .map_err(|e| anyhow::anyhow!("{e:?}"))?
            .with_evaluator(evaluate_source);
        let initial = font_receiver.latest().unwrap().unwrap();
        drop(updates);
        sender
            .try_send(OwnerEvent::Evaluate {
                request_id: 8402,
                source: "(insert \"q\")".into(),
            })
            .unwrap();
        let step = worker.try_step().unwrap();
        assert!(step.evaluation.unwrap().outcome.is_ok());
        assert!(step.edit.unwrap());
        assert!(step.render_error.is_some());
        let visible = font_receiver.latest().unwrap().unwrap();
        assert!(visible.frame().generation > initial.frame().generation);
        assert!(Arc::ptr_eq(
            &visible,
            &worker.bridge().prepared_font_plan().unwrap()
        ));
        assert!(worker.retry_frame().is_err());
        assert!(
            font_receiver.latest().unwrap().unwrap().frame().generation
                > visible.frame().generation
        );
        let s = worker
            .bridge()
            .snapshot()
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let v = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        assert_eq!(
            s.buffers
                .iter()
                .find(|b| b.id == v.view.buffer)
                .unwrap()
                .document
                .to_text(),
            "q"
        );
        Ok(())
    })
    .unwrap();
}
