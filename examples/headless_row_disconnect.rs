//! Actual Entity payload Drop closes UI endpoint and owner revokes on observation.
use gpui::{AppContext, Application};
use rune_display::{
    TextSnapshot,
    retained_row_handoff::{RetainedRowReply, RowPreparationUi, row_preparation_handoff},
    retained_row_transaction::{RetainedRowKey, RetainedRowScope},
};
use rune_editor::{RopeDocument, Session};
use rune_text::{PropSetId, Rope, Snapshot};
use std::{cell::Cell, process::ExitCode, rc::Rc, sync::mpsc, time::Duration};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// UI endpoint remains inside the GPUI entity, not a worker or global registry.
struct UiRowPreparation {
    endpoint: Option<RowPreparationUi>,
    expected: Option<RetainedRowKey>,
    dropped: mpsc::SyncSender<()>,
}

impl UiRowPreparation {
    fn dispatch(&mut self) -> Result<(), String> {
        let request = self.endpoint.as_ref().ok_or("endpoint closed")?.next_live_request()
            .map_err(|error| format!("receive={error:?}"))?
            .ok_or("missing request")?;
        if request.text() != "a\u{301}b" || request.source_range() != (0..3) {
            return Err("source changed".into());
        }
        // A diagnostic registry number, NOT a native line or font handle.
        self.endpoint.as_ref().ok_or("endpoint closed")?.reply(RetainedRowReply { key: request.key(), ui_token: 42 })
            .map_err(|error| format!("reply={:?}", error.error))?;
        self.expected = Some(request.key());
        Ok(())
    }

    fn observe_acceptance(&self) -> Result<(), String> {
        let token = self.endpoint.as_ref().ok_or("endpoint closed")?
            .next_live_acceptance().map_err(|error| format!("acceptance={error:?}"))?
            .ok_or("missing owner acceptance")?;
        if Some(token.key()) != self.expected || token.ui_token() != 42 || !token.is_live() {
            return Err("owner acceptance did not match retained request".into());
        }
        Ok(())
    }
}


impl Drop for UiRowPreparation {
    fn drop(&mut self) {
        // Notify only after the endpoint's own disconnect publication.
        drop(self.endpoint.take());
        let _ = self.dropped.try_send(());
    }
}

fn main() -> ExitCode {
    let hold_entity = std::env::args().skip(1).any(|argument| argument == "--hold-entity");
    let status = Rc::new(Cell::new(2));
    let outcome = status.clone();
    let (mut preparation, ui) = match row_preparation_handoff() {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("HEADLESS_ROW_HANDOFF_ERROR construction={error:?}");
            return ExitCode::FAILURE;
        }
    };
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
    let (accepted_sender, accepted_receiver) = mpsc::sync_channel(1);
    let (dropped_sender, dropped_receiver) = mpsc::sync_channel(1);
    let owner = std::thread::spawn(move || -> Result<(), String> {
        let document = RopeDocument::new(
            Snapshot::new(Rope::from("a\u{301}b")), PropSetId(0),
        );
        let editor = Session::new(document, 80, 24).map_err(|error| error.to_string())?;
        let before = editor.snapshot();
        let buffer = &before.buffers[0];
        let source = buffer.display_snapshot();
        let scope = RetainedRowScope {
            sequence: before.sequence,
            window: before.selected_window.as_u64(),
            buffer: buffer.id.as_u64(),
            text_epoch: buffer.version,
            source_start_scalar: 0,
            style_revision: 1, viewport_revision: 1,
            font_bank_generation: 1, shaping_policy_revision: 1,
        };
        preparation.prepare_snapshot(scope, &source).map_err(|error| format!("prepare={error:?}"))?;
        ready_sender.send(()).map_err(|_| "ready receiver dropped")?;
        reply_receiver.recv_timeout(HANDSHAKE_TIMEOUT).map_err(|_| "foreground reply deadline")?;
        let text: String = source.chars_from(0).collect();
        let accepted = preparation.poll(scope, &text).map_err(|error| format!("poll={error:?}"))?
            .ok_or("reply not delivered")?;
        if !accepted.is_live() || accepted.ui_token() != 42
            || editor.snapshot().sequence != before.sequence || editor.point() != 0
        {
            return Err("correlation or no-mutation assertion".into());
        }
        accepted_sender.send(()).map_err(|_| "acceptance signal disconnected")?;
        dropped_receiver.recv_timeout(HANDSHAKE_TIMEOUT).map_err(|_| "entity payload drop deadline")?;
        if !matches!(preparation.poll(scope, &text),
            Err(rune_display::retained_row_handoff::RowHandoffError::Disconnected))
            || accepted.is_live()
        {
            return Err("owner did not observe disconnect and revoke".into());
        }
        Ok(())
    });
    if ready_receiver.recv_timeout(HANDSHAKE_TIMEOUT).is_err() {
        eprintln!("HEADLESS_ROW_HANDOFF_ERROR owner readiness");
        let _ = owner.join();
        return ExitCode::FAILURE;
    }
    Application::headless().run(move |app| {
        let ui_entity = app.new(|_| UiRowPreparation { endpoint: Some(ui), expected: None, dropped: dropped_sender });
        let weak_entity = ui_entity.downgrade();
        app.spawn(async move |app| {
            let dispatch = app.update(|cx| {
                ui_entity.update(cx, |state, _| state.dispatch())
            }).map_err(|error| format!("entity update={error}"))
                .and_then(|result| result)
                .and_then(|()| reply_sender.send(())
                    .map_err(|_| "owner signal disconnected".to_owned()));
            let accepted_signal = app.background_executor().spawn(async move {
                accepted_receiver.recv_timeout(HANDSHAKE_TIMEOUT)
            }).await;
            let receipt = app.update(|cx| {
                ui_entity.update(cx, |state, _| state.observe_acceptance())
            }).map_err(|error| format!("acceptance entity update={error}"))
                .and_then(|result| result);
            let retained = weak_entity.upgrade().is_some();
            let mut retained_entity = Some(ui_entity);
            if !hold_entity {
                drop(retained_entity.take());
            }
            let dead = weak_entity.upgrade().is_none();
            // Run a genuine App update/effect cycle so released entity payloads
            // can be destroyed. This does not create or simulate a Window.
            let flushed = app.update(|_| ()).is_ok();
            let joined = app.background_executor().spawn(async move { owner.join() }).await;
            drop(retained_entity.take());
            let _ = app.update(|_| ());
            let result = dispatch.and(receipt)
                .and_then(|_| accepted_signal.map_err(|_| "acceptance deadline".to_owned()))
                .and_then(|_| joined.map_err(|_| "owner panic".to_owned())?)
                .and_then(|_| if retained && dead && flushed { Ok(()) }
                    else { Err("entity lifecycle check failed".into()) });
            match result {
                Ok(()) => {
                    println!("HEADLESS_ROW_ENTITY_DISCONNECT_PASS owner_receipt_verified=true live_drain=true entity_payload_drop=true endpoint_disconnected=true owner_observed=true accepted_revoked=true weak_dead=true window=false shaping=false paint=false");
                    outcome.set(0);
                }
                Err(error) => {
                    eprintln!("HEADLESS_ROW_HANDOFF_ERROR {error}");
                    outcome.set(1);
                }
            }
            let _ = app.update(|app| rune_gpui::quit_on_next_foreground_turn(app));
        }).detach();
    });
    ExitCode::from(status.get())
}
