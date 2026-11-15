//! Real GPUI activity awaits with an independently deadline-controlled owner.
//! No Window, native row, geometry, shaping or painting is constructed.
use gpui::Application;
use rune_display::{
    TextSnapshot,
    retained_row_handoff::{
        RowActivity, RowHandoffError, RowPreparationOwner, RowPreparationUi,
        RetainedRowReply, row_preparation_handoff,
    },
    retained_row_transaction::RetainedRowScope,
};
use rune_editor::{RopeDocument, Session};
use rune_text::{PropSetId, Rope, Snapshot};
use std::{
    cell::Cell, future::Future, pin::pin, process::ExitCode, rc::Rc,
    sync::{Arc, mpsc}, task::{Context, Poll, Wake, Waker},
    thread, time::{Duration, Instant},
};

const OWNER_DEADLINE: Duration = Duration::from_secs(2);
const DIAGNOSTIC_UI_TOKEN: u64 = 42;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Normal, CloseBeforeRequest, SuppressReply }

struct Unpark(thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) { self.0.unpark(); }
    fn wake_by_ref(self: &Arc<Self>) { self.0.unpark(); }
}

fn owner_activity(owner: &mut RowPreparationOwner, deadline: Instant) -> Result<RowActivity, String> {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut wait = pin!(owner.wait_for_activity());
    loop {
        if let Poll::Ready(activity) = wait.as_mut().poll(&mut Context::from_waker(&waker)) {
            return Ok(activity);
        }
        let remaining = deadline.checked_duration_since(Instant::now())
            .ok_or("owner activity deadline")?;
        // unpark-before-park leaves a token, so registration cannot lose the wake.
        thread::park_timeout(remaining);
    }
}

fn run_owner(mut preparation: RowPreparationOwner, start: mpsc::Receiver<()>, mode: Mode)
    -> Result<(), String>
{
    // This thread independently closes on every return path (endpoint Drop).
    start.recv_timeout(OWNER_DEADLINE).map_err(|_| "owner start deadline")?;
    let deadline = Instant::now() + OWNER_DEADLINE;
    if mode == Mode::CloseBeforeRequest {
        preparation.close();
        return Ok(());
    }
    let document = RopeDocument::new(Snapshot::new(Rope::from("a\u{301}b")), PropSetId(0));
    let editor = Session::new(document, 80, 24).map_err(|error| error.to_string())?;
    let snapshot = editor.snapshot();
    let buffer = &snapshot.buffers[0];
    let source = buffer.display_snapshot();
    let scope = RetainedRowScope {
        sequence: snapshot.sequence, window: snapshot.selected_window.as_u64(),
        buffer: buffer.id.as_u64(), text_epoch: source.revision(), source_start_scalar: 0,
        style_revision: 1, viewport_revision: 1, font_bank_generation: 1,
        shaping_policy_revision: 1,
    };
    let text: String = source.chars_from(0).collect();
    preparation.prepare_snapshot(scope, &source).map_err(|error| format!("prepare={error:?}"))?;
    let accepted = loop {
        match preparation.poll(scope, &text).map_err(|error| format!("poll={error:?}"))? {
            Some(token) => break token,
            None => {
                if owner_activity(&mut preparation, deadline)? == RowActivity::Disconnected {
                    return Err("UI closed before acceptance".into());
                }
            }
        }
    };
    loop {
        if owner_activity(&mut preparation, deadline)? == RowActivity::Disconnected {
            if !matches!(preparation.poll(scope, &text), Err(RowHandoffError::Disconnected)) {
                return Err("owner failed to observe UI close".into());
            }
            break;
        }
        // Poll drains any reply backlog before re-registering the coalesced edge.
        preparation.poll(scope, &text).map_err(|error| format!("post-accept poll={error:?}"))?;
    }
    if accepted.is_live() || editor.point() != 0 || editor.snapshot().sequence != snapshot.sequence {
        return Err("revocation/no-owner-mutation assertion".into());
    }
    Ok(())
}

async fn register_then_start(
    ui: &mut RowPreparationUi,
    sender: mpsc::SyncSender<()>,
) -> Result<RowActivity, String> {
    let mut sender = Some(sender);
    let mut wait = pin!(ui.wait_for_activity());
    std::future::poll_fn(|context| {
        let polled = wait.as_mut().poll(context);
        if let Some(sender) = sender.take() {
            if !polled.is_pending() {
                return Poll::Ready(Err("first activity wait was not initially idle".into()));
            }
            // The REAL foreground task Waker is registered before the owner runs.
            // This channel starts the independent owner; it never wakes the UI.
            if sender.send(()).is_err() {
                return Poll::Ready(Err("start disconnected".into()));
            }
        }
        polled.map(Ok)
    }).await
}

async fn serve_ui(ui: &mut RowPreparationUi, mode: Mode) -> Result<(), String> {
    let mut expected = None;
    loop {
        // Drain BOTH applicable mailboxes before await. Activity is not level-triggered.
        match ui.next_live_acceptance() {
            Ok(Some(token)) => {
                if Some(token.key()) != expected || token.ui_token() != DIAGNOSTIC_UI_TOKEN {
                    return Err("uncorrelated owner receipt".into());
                }
                if mode != Mode::Normal { return Err("unexpected acceptance".into()); }
                return Ok(());
            }
            Ok(None) => {}
            Err(RowHandoffError::Disconnected) if mode == Mode::CloseBeforeRequest => return Ok(()),
            Err(error) => return Err(format!("acceptance={error:?}")),
        }
        match ui.next_live_request() {
            Ok(Some(request)) => {
                if request.text() != "a\u{301}b" || request.source_range() != (0..3) {
                    return Err("request source changed".into());
                }
                expected = Some(request.key());
                if mode != Mode::SuppressReply {
                    ui.reply(RetainedRowReply {
                        key: request.key(), ui_token: DIAGNOSTIC_UI_TOKEN,
                    }).map_err(|error| format!("reply={:?}", error.error))?;
                }
            }
            Ok(None) => {}
            Err(RowHandoffError::Disconnected) if mode == Mode::CloseBeforeRequest => return Ok(()),
            Err(error) => return Err(format!("request={error:?}")),
        }
        if ui.wait_for_activity().await == RowActivity::Disconnected {
            if mode == Mode::CloseBeforeRequest { return Ok(()); }
            return Err("independent owner close observed".into());
        }
    }
}

fn main() -> ExitCode {
    let mode = match std::env::args().nth(1).as_deref() {
        None => Mode::Normal,
        Some("--close-before-request") => Mode::CloseBeforeRequest,
        Some("--suppress-reply") => Mode::SuppressReply,
        _ => return ExitCode::FAILURE,
    };
    let (preparation, mut ui) = match row_preparation_handoff() {
        Ok(pair) => pair,
        Err(_) => return ExitCode::FAILURE,
    };
    let (start_sender, start_receiver) = mpsc::sync_channel(1);
    let owner = thread::spawn(move || run_owner(preparation, start_receiver, mode));
    let status = Rc::new(Cell::new(2));
    let outcome = status.clone();
    Application::headless().run(move |app| {
        app.spawn(async move |app| {
            let started = register_then_start(&mut ui, start_sender).await;
            let result = match started {
                Ok(RowActivity::Ready) => serve_ui(&mut ui, mode).await,
                Ok(RowActivity::Disconnected) if mode == Mode::CloseBeforeRequest => Ok(()),
                Ok(RowActivity::Disconnected) => Err("owner closed during registered start".into()),
                Err(error) => Err(error),
            };
            ui.close();
            let joined = app.background_executor().spawn(async move { owner.join() }).await;
            let result = result.and_then(|_| joined.map_err(|_| "owner panic".to_owned())?);
            match result {
                Ok(()) => {
                    println!("HEADLESS_ROW_ACTIVITY_PASS close_before_request={} actual_foreground_await=true registered_before_owner_start=true independent_owner_shutdown=true owner_park_unpark=true window=false shape=false paint=false", mode == Mode::CloseBeforeRequest);
                    outcome.set(0);
                }
                Err(error) => {
                    eprintln!("HEADLESS_ROW_ACTIVITY_ERROR suppress_reply={} reason={error}", mode == Mode::SuppressReply);
                    outcome.set(1);
                }
            }
            let _ = app.update(|app| rune_gpui::quit_on_next_foreground_turn(app));
        }).detach();
    });
    ExitCode::from(status.get())
}
