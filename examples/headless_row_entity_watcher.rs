//! Actual headless WeakEntity watcher, with independently controlled owner close.
use gpui::{AppContext, Application, AsyncApp, WeakEntity};
use rune_display::{
    TextSnapshot,
    retained_row_handoff::{RowActivity, RowHandoffError, RowPreparationOwner, RowPreparationUi, RetainedRowReply, row_preparation_handoff},
    retained_row_transaction::RetainedRowScope,
};
use rune_editor::{RopeDocument, Session};
use rune_text::{PropSetId, Rope, Snapshot};
use std::{
    cell::Cell, future::Future, pin::pin, process::ExitCode, rc::Rc,
    sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc},
    task::{Context, Poll, Wake, Waker}, thread, time::{Duration, Instant},
};

const OWNER_LIMIT: Duration = Duration::from_secs(2);
const OBSERVER_LIMIT: Duration = Duration::from_secs(3);
const FIXTURE_TOKEN: u64 = 42;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Normal, QueuedReceipt, KeepEntity, SuppressCancel }

struct Projection {
    receipts: usize,
    dropped: Arc<AtomicBool>,
}
impl Drop for Projection {
    fn drop(&mut self) { self.dropped.store(true, Ordering::Release); }
}

struct Unpark(thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) { self.0.unpark(); }
    fn wake_by_ref(self: &Arc<Self>) { self.0.unpark(); }
}

fn wait_owner(owner: &mut RowPreparationOwner, stop: &AtomicBool, deadline: Instant)
    -> Result<RowActivity, String>
{
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut wait = pin!(owner.wait_for_activity());
    loop {
        // Check even on continuous Ready; not only on the parking branch.
        if stop.load(Ordering::Acquire) { return Err("controller stop".into()); }
        let remaining = deadline.checked_duration_since(Instant::now())
            .ok_or("owner absolute deadline")?;
        match wait.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Ready(activity) => return Ok(activity),
            Poll::Pending => thread::park_timeout(remaining),
        }
    }
}

fn run_owner(
    mut owner: RowPreparationOwner,
    start: mpsc::Receiver<()>,
    accepted_notice: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    start.recv_timeout(OBSERVER_LIMIT).map_err(|_| "owner start deadline")?;
    let deadline = Instant::now() + OWNER_LIMIT;
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
    owner.prepare_snapshot(scope, &source).map_err(|error| format!("prepare={error:?}"))?;
    let token = loop {
        if stop.load(Ordering::Acquire) { return Err("stopped before acceptance".into()); }
        if Instant::now() >= deadline { return Err("owner absolute deadline".into()); }
        if let Some(token) = owner.poll(scope, &text).map_err(|error| format!("poll={error:?}"))? {
            break token;
        }
        if wait_owner(&mut owner, &stop, deadline)? == RowActivity::Disconnected {
            return Err("UI disconnected before acceptance".into());
        }
    };
    // Receipt was queued before this notice. Used only by the queued-backlog control.
    accepted_notice.send(()).map_err(|_| "acceptance observer disconnected")?;
    loop {
        if stop.load(Ordering::Acquire) {
            owner.close();
            if token.is_live() || editor.point() != 0 || editor.snapshot().sequence != snapshot.sequence {
                return Err("controlled close/no-mutation assertion".into());
            }
            return Ok(());
        }
        if Instant::now() >= deadline { return Err("owner absolute deadline".into()); }
        match wait_owner(&mut owner, &stop, deadline) {
            Ok(RowActivity::Ready) => {
                owner.poll(scope, &text).map_err(|error| format!("drain={error:?}"))?;
            }
            Ok(RowActivity::Disconnected) => return Err("UI disconnected before controller".into()),
            Err(_) if stop.load(Ordering::Acquire) => continue,
            Err(error) => return Err(error),
        }
    }
    // Endpoint Drop also closes/wakes on every error path.
}

async fn register_start(ui: &mut RowPreparationUi, sender: mpsc::SyncSender<()>) -> Result<(), String> {
    let mut sender = Some(sender);
    let mut wait = pin!(ui.wait_for_activity());
    let activity = std::future::poll_fn(|context| -> Poll<Result<RowActivity, String>> {
        let polled = wait.as_mut().poll(context);
        if let Some(sender) = sender.take() {
            if !polled.is_pending() { return Poll::Ready(Err("initial wait not idle".into())); }
            if sender.send(()).is_err() { return Poll::Ready(Err("start failed".into())); }
        }
        polled.map(Ok)
    }).await?;
    if activity != RowActivity::Ready { return Err("owner closed at start".into()); }
    Ok(())
}

async fn park_then_observe_close(ui: &mut RowPreparationUi, parked: mpsc::SyncSender<()>)
    -> Result<(), String>
{
    let mut parked = Some(parked);
    loop {
        let mut wait = pin!(ui.wait_for_activity());
        let activity = std::future::poll_fn(|context| -> Poll<Result<RowActivity, String>> {
            let polled = wait.as_mut().poll(context);
            if polled.is_pending() {
                if let Some(sender) = parked.take() {
                    if sender.send(()).is_err() { return Poll::Ready(Err("controller dropped".into())); }
                }
            }
            polled.map(Ok)
        }).await?;
        if activity == RowActivity::Disconnected { return Ok(()); }
        // Only finite stale capacity edges remain in this one-row cancellation phase.
    }
}

async fn watch(
    ui: &mut RowPreparationUi,
    weak: &WeakEntity<Projection>,
    mode: Mode,
    parked: mpsc::SyncSender<()>,
    accepted_notice: mpsc::Receiver<()>,
    app: &mut AsyncApp,
) -> Result<(), String> {
    let mut expected = None;
    loop {
        // Normal driver drains both queues. QueuedReceipt deliberately retains
        // an already-published receipt to test shutdown, never to present it.
        if mode != Mode::QueuedReceipt {
            if let Some(token) = ui.next_live_acceptance().map_err(|error| format!("receipt={error:?}"))? {
                if Some(token.key()) != expected || token.ui_token() != FIXTURE_TOKEN {
                    return Err("receipt mismatch".into());
                }
                app.update(|cx| {
                    let entity = weak.upgrade().ok_or("entity vanished before projection")?;
                    entity.update(cx, |state, _| state.receipts += 1);
                    Ok::<(), &'static str>(())
                }).map_err(|error| error.to_string())?.map_err(str::to_owned)?;
                break;
            }
        }
        if let Some(request) = ui.next_live_request().map_err(|error| format!("request={error:?}"))? {
            if request.text() != "a\u{301}b" || request.source_range() != (0..3) { return Err("source mismatch".into()); }
            expected = Some(request.key());
            ui.reply(RetainedRowReply { key: request.key(), ui_token: FIXTURE_TOKEN })
                .map_err(|error| format!("reply={:?}", error.error))?;
            if mode == Mode::QueuedReceipt {
                app.background_executor().spawn(async move {
                    accepted_notice.recv_timeout(OBSERVER_LIMIT)
                }).await.map_err(|_| "queued receipt not confirmed")?;
                break;
            }
        }
        if ui.wait_for_activity().await == RowActivity::Disconnected { return Err("early close".into()); }
    }
    park_then_observe_close(ui, parked).await?;
    if weak.upgrade().is_some() { return Err("entity still alive at controlled close".into()); }
    // Closure observation drains the deliberately queued receipt, without projection.
    if !matches!(ui.next_live_acceptance(), Err(RowHandoffError::Disconnected)) {
        return Err("receipt mailbox did not close".into());
    }
    Ok(())
}

fn main() -> ExitCode {
    let mode = match std::env::args().nth(1).as_deref() {
        None => Mode::Normal,
        Some("--queued-receipt") => Mode::QueuedReceipt,
        Some("--keep-entity") => Mode::KeepEntity,
        Some("--suppress-cancel") => Mode::SuppressCancel,
        _ => return ExitCode::FAILURE,
    };
    let (owner, mut ui) = match row_preparation_handoff() { Ok(pair) => pair, Err(_) => return ExitCode::FAILURE };
    let stop = Arc::new(AtomicBool::new(false));
    let owner_stop = stop.clone();
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
    let (parked_tx, parked_rx) = mpsc::sync_channel(1);
    let owner_thread = thread::spawn(move || run_owner(owner, start_rx, accepted_tx, owner_stop));
    let owner_unparker = owner_thread.thread().clone();
    let dropped = Arc::new(AtomicBool::new(false));
    let status = Rc::new(Cell::new(2));
    let outcome = status.clone();
    Application::headless().run(move |app| {
        let entity = app.new(|_| Projection { receipts: 0, dropped: dropped.clone() });
        let weak = entity.downgrade();
        let watcher = app.spawn(async move |app| {
            let result = match register_start(&mut ui, start_tx).await {
                Ok(()) => watch(&mut ui, &weak, mode, parked_tx, accepted_rx, app).await,
                Err(error) => Err(error),
            };
            ui.close();
            result
        });
        app.spawn(async move |app| {
            let parked = app.background_executor().spawn(async move {
                parked_rx.recv_timeout(OBSERVER_LIMIT)
            }).await;
            let expected_receipts = usize::from(mode != Mode::QueuedReceipt);
            let projected = app.update(|cx| entity.read(cx).receipts).unwrap_or(usize::MAX);
            let mut strong = Some(entity);
            if mode != Mode::KeepEntity { drop(strong.take()); }
            let flushed = app.update(|_| ()).is_ok();
            if mode != Mode::SuppressCancel {
                stop.store(true, Ordering::Release);
                owner_unparker.unpark();
            }
            let watched = watcher.await;
            let joined = app.background_executor().spawn(async move { owner_thread.join() }).await;
            drop(strong.take());
            let _ = app.update(|_| ());
            let result = parked.map_err(|_| "watcher did not park".to_owned())
                .and(watched)
                .and_then(|_| joined.map_err(|_| "owner panic".to_owned())?)
                .and_then(|_| if projected == expected_receipts && flushed && dropped.load(Ordering::Acquire) {
                    Ok(())
                } else { Err("projection/payload-drop assertion".into()) });
            match result {
                Ok(()) => {
                    println!("HEADLESS_WEAK_ENTITY_WATCHER_PASS queued_receipt={} parked_before_controller=true weak_entity_gone=true payload_drop_observed=true independent_owner_cancel=true all_loop_deadline=true window=false shape=false paint=false", mode == Mode::QueuedReceipt);
                    outcome.set(0);
                }
                Err(error) => {
                    eprintln!("HEADLESS_WEAK_ENTITY_WATCHER_ERROR keep_entity={} suppress_cancel={} reason={error}", mode == Mode::KeepEntity, mode == Mode::SuppressCancel);
                    outcome.set(1);
                }
            }
            let _ = app.update(|app| rune_gpui::quit_on_next_foreground_turn(app));
        }).detach();
    });
    ExitCode::from(status.get())
}
