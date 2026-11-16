//! Opt-in real Window diagnostic. Never run on a substitute display backend.
//! Compile qualification is not evidence that Window/shape/paint executed.
use gpui::{App, AppContext, Application, Context, IntoElement, ParentElement, Render,
    Styled, Window, WindowOptions, div, font, px};
use rune_display::{TextSnapshot, retained_row_handoff::{RowActivity, RowPreparationOwner,
    RowPreparationUi, row_preparation_handoff}, retained_row_transaction::{AcceptedRowToken,
    RetainedRowScope}};
use rune_editor::{RopeDocument, Session};
use rune_text::{PropSetId, Rope, Snapshot};
use rune_gpui::{NativeFont, NativeFontVariantBank, retained_row_stage::{
    DiagnosticRowPaintStatus, StagedNativeRow}};
use std::{cell::{Cell, RefCell}, future::Future, pin::pin, process::ExitCode, rc::Rc, sync::Arc,
    task::{Context as TaskContext, Poll, Wake, Waker}, thread, time::{Duration, Instant}};

const DEADLINE: Duration = Duration::from_secs(3);
struct Unpark(thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) { self.0.unpark(); }
    fn wake_by_ref(self: &Arc<Self>) { self.0.unpark(); }
}

// Independent opposite endpoint controller: no foreground task is needed to expire it.
fn owner(mut endpoint: RowPreparationOwner, bank_id: u64) -> Result<(), String> {
    let editor = Session::new(RopeDocument::new(
        Snapshot::new(Rope::from("AVATAR")), PropSetId(0)), 80, 24)
        .map_err(|error| error.to_string())?;
    let snapshot = editor.snapshot();
    let buffer = &snapshot.buffers[0];
    let source = buffer.display_snapshot();
    let scope = RetainedRowScope {
        sequence: snapshot.sequence, window: snapshot.selected_window.as_u64(),
        buffer: buffer.id.as_u64(), text_epoch: source.revision(), source_start_scalar: 0,
        style_revision: 1, viewport_revision: 1, font_bank_generation: bank_id,
        shaping_policy_revision: 1,
    };
    let text: String = source.chars_from(0).collect();
    endpoint.prepare_snapshot(scope, &source).map_err(|error| format!("prepare: {error:?}"))?;
    let deadline = Instant::now() + DEADLINE;
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut accepted = false;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            endpoint.close();
            return if accepted { Ok(()) } else { Err("owner acceptance deadline".into()) };
        };
        match endpoint.poll(scope, &text) {
            Ok(token) => accepted |= token.is_some(),
            Err(rune_display::retained_row_handoff::RowHandoffError::Disconnected) => {
                return Err("UI disconnected".into());
            }
            Err(error) => return Err(format!("poll: {error:?}")),
        }
        let mut wait = pin!(endpoint.wait_for_activity());
        match wait.as_mut().poll(&mut TaskContext::from_waker(&waker)) {
            Poll::Ready(RowActivity::Disconnected) => return Err("UI disconnected".into()),
            Poll::Ready(RowActivity::Ready) => {}
            Poll::Pending => thread::park_timeout(remaining),
        }
    }
}

#[derive(Clone)]
struct PaintAttempt {
    render_number: u64,
    ui_token: u64,
    status: Rc<Cell<DiagnosticRowPaintStatus>>,
}

type PaintJournal = Rc<RefCell<Option<PaintAttempt>>>;

struct RowView {
    stage: Option<Rc<StagedNativeRow>>,
    receipt: Option<AcceptedRowToken>,
    journal: PaintJournal,
    render_number: u64,
}
impl Render for RowView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut root = div().w(px(360.0)).h(px(50.0));
        // Each render owns a fresh callback cell. An old canvas cannot update it.
        *self.journal.borrow_mut() = None;
        let Some(render_number) = self.render_number.checked_add(1) else { return root; };
        self.render_number = render_number;
        if let (Some(stage), Some(receipt)) = (&self.stage, &self.receipt) {
            let attempt = PaintAttempt {
                render_number, ui_token: receipt.ui_token(),
                status: Rc::new(Cell::new(DiagnosticRowPaintStatus::NotInvoked)),
            };
            *self.journal.borrow_mut() = Some(attempt.clone());
            root = root.child(stage.clone().diagnostic_canvas(
                receipt.clone(), 32.0, attempt.status).w(px(360.0)).h(px(50.0)));
        }
        root
    }
}

async fn serve(ui: &mut RowPreparationUi, handle: gpui::WindowHandle<RowView>,
    baseline: NativeFont, bank: Rc<NativeFontVariantBank>, app: &mut gpui::AsyncApp)
    -> Result<(), String>
{
    loop {
        // Activity coalesces: drain both bounded mailboxes before awaiting.
        match ui.next_live_acceptance() {
            Ok(Some(receipt)) => handle.update(app, |view, _, cx| {
                view.receipt = Some(receipt); cx.notify();
            }).map_err(|error| error.to_string())?,
            Ok(None) => {}
            Err(error) => return Err(format!("receipt: {error:?}")),
        }
        match ui.next_live_request() {
            Ok(Some(request)) => {
                let stage = handle.update(app, |_, window, cx| {
                    StagedNativeRow::shape(request, &baseline, bank.clone(), gpui::black(), window, cx)
                }).map_err(|error| error.to_string())?.map_err(str::to_owned)?;
                let reply = stage.observation().reply;
                handle.update(app, |view, _, cx| {
                    view.stage = Some(Rc::new(stage)); view.receipt = None; cx.notify();
                }).map_err(|error| error.to_string())?;
                // No Entity/Window borrow crosses this synchronous wake-producing send.
                ui.reply(reply).map_err(|error| format!("reply: {:?}", error.error))?;
            }
            Ok(None) => {}
            Err(error) => return Err(format!("request: {error:?}")),
        }
        if ui.wait_for_activity().await == RowActivity::Disconnected { return Ok(()); }
    }
}

fn diagnostic_exit_code(ui_ok: bool, owner_ok: bool,
    paint_status: Option<DiagnosticRowPaintStatus>) -> u8
{
    if ui_ok && owner_ok && paint_status == Some(DiagnosticRowPaintStatus::PaintReturnedOk) {
        0
    } else {
        1
    }
}

fn main() -> ExitCode {
    let exit = Rc::new(Cell::new(2u8));
    let outcome = exit.clone();
    Application::new().run(move |app: &mut App| {
        let setup = (|| -> Result<_, String> {
            let baseline = NativeFont::measure(font("DejaVu Sans Mono"), 17.0, 3.0, app)
                .map_err(str::to_owned)?;
            let bank = Rc::new(NativeFontVariantBank::measure(&baseline, app).map_err(str::to_owned)?);
            let bank_id = bank.capabilities(app).map_err(str::to_owned)?.bank_id;
            let (owner_endpoint, ui) = row_preparation_handoff().map_err(|error| format!("{error:?}"))?;
            let journal: PaintJournal = Rc::new(RefCell::new(None));
            let handle = app.open_window(WindowOptions::default(), |_, app| {
                app.new(|_| RowView { stage: None, receipt: None,
                    journal: journal.clone(), render_number: 0 })
            }).map_err(|error| error.to_string())?;
            let owner_thread = thread::spawn(move || owner(owner_endpoint, bank_id));
            Ok((baseline, bank, ui, journal, handle, owner_thread))
        })();
        let Ok((baseline, bank, mut ui, journal, handle, owner_thread)) = setup else {
            outcome.set(1);
            eprintln!("WINDOW_ROW_DIAGNOSTIC_SETUP_ERROR");
            rune_gpui::quit_on_next_foreground_turn(app); return;
        };
        app.spawn(async move |app| {
            let ui_result = serve(&mut ui, handle, baseline, bank, app).await;
            ui.close();
            let joined = app.background_executor().spawn(async move { owner_thread.join() }).await;
            let attempt = journal.borrow().clone();
            let owner_ok = matches!(&joined, Ok(Ok(())));
            outcome.set(diagnostic_exit_code(ui_result.is_ok(), owner_ok,
                attempt.as_ref().map(|attempt| attempt.status.get())));
            eprintln!("WINDOW_ROW_DIAGNOSTIC_END ui={ui_result:?} owner={joined:?} latest_render={:?} stage_token={:?} paint_status={:?} pixels_verified=false",
                attempt.as_ref().map(|a| a.render_number),
                attempt.as_ref().map(|a| a.ui_token),
                attempt.as_ref().map(|a| a.status.get()));
            if app.update(|app| rune_gpui::quit_on_next_foreground_turn(app)).is_err() {
                outcome.set(1);
            }
        }).detach();
    });
    ExitCode::from(exit.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune_display::retained_row_handoff::RetainedRowReply;

    #[test]
    fn actual_session_owner_accepts_one_protocol_reply_then_observes_ui_close() {
        let (endpoint, mut ui) = row_preparation_handoff().unwrap();
        let owner_thread = thread::spawn(move || owner(endpoint, 7));
        let deadline = Instant::now() + DEADLINE;
        let request = loop {
            if let Some(request) = ui.next_live_request().unwrap() { break request; }
            assert!(Instant::now() < deadline, "owner did not submit request");
            thread::yield_now();
        };
        assert_eq!(request.text(), "AVATAR");
        assert_eq!(request.source_range(), 0..6);
        // Protocol-only token, deliberately not a native stage or font proof.
        ui.reply(RetainedRowReply { key: request.key(), ui_token: 42 }).unwrap();
        let receipt = loop {
            if let Some(receipt) = ui.next_live_acceptance().unwrap() { break receipt; }
            assert!(Instant::now() < deadline, "owner did not accept reply");
            thread::yield_now();
        };
        assert_eq!(receipt.key(), request.key());
        ui.close();
        assert_eq!(owner_thread.join().unwrap(), Err("UI disconnected".into()));
        assert!(!receipt.is_live());
    }

    #[test]
    fn independent_owner_deadline_closes_when_ui_never_replies() {
        let (endpoint, ui) = row_preparation_handoff().unwrap();
        let owner_thread = thread::spawn(move || owner(endpoint, 7));
        assert_eq!(owner_thread.join().unwrap(), Err("owner acceptance deadline".into()));
        assert!(ui.next_live_request().is_err());
    }


    #[test]
    fn old_callback_status_cannot_overwrite_a_new_render_attempt() {
        let old = PaintAttempt { render_number: 1, ui_token: 10,
            status: Rc::new(Cell::new(DiagnosticRowPaintStatus::NotInvoked)) };
        let current = PaintAttempt { render_number: 2, ui_token: 11,
            status: Rc::new(Cell::new(DiagnosticRowPaintStatus::NotInvoked)) };
        let journal: PaintJournal = Rc::new(RefCell::new(Some(current)));
        old.status.set(DiagnosticRowPaintStatus::PaintReturnedOk);
        let current = journal.borrow();
        let current = current.as_ref().unwrap();
        assert_eq!(current.render_number, 2);
        assert_eq!(current.ui_token, 11);
        assert_eq!(current.status.get(), DiagnosticRowPaintStatus::NotInvoked);
    }

    #[test]
    fn exit_zero_requires_both_protocol_successes_and_latest_sdk_paint_ok() {
        for ui_ok in [false, true] {
            for owner_ok in [false, true] {
                for status in [None, Some(DiagnosticRowPaintStatus::NotInvoked),
                    Some(DiagnosticRowPaintStatus::Rejected),
                    Some(DiagnosticRowPaintStatus::PaintReturnedError),
                    Some(DiagnosticRowPaintStatus::PaintReturnedOk)]
                {
                    assert_eq!(diagnostic_exit_code(ui_ok, owner_ok, status) == 0,
                        ui_ok && owner_ok && status == Some(DiagnosticRowPaintStatus::PaintReturnedOk));
                }
            }
        }
    }

}
