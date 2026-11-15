//! Ordered bounded owner handoff, explicit composition outcome and startup gate.
//! No platform InputHandler or FrameView envelope wiring is implied.
use crate::{
    composition::{CommittedText, CompositionToken},
    editor_bridge::{BridgeError, EditorBridge},
};
use rune_display::{FrameDisplay, HostPublisher, InputEvent};
use rune_editor::{DisplayDocument, EditorHost};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
};
#[derive(Debug)]
pub struct CompositionRequest {
    pub token: CompositionToken,
    pub committed: CommittedText,
}
#[derive(Debug)]
pub enum OwnerEvent {
    /// Payload-free internal scheduling hint; it cannot carry editor authority.
    RowWake(RowWakeToken),
    Input(InputEvent),
    NativeKey {
        request: NativeKeyRequest,
        fallback: Option<rune_editor::Command>,
    },
    KeyText {
        window_id: u64,
        buffer_id: u64,
        text: String,
    },
    ScrollWindow {
        window_id: u64,
        buffer_id: u64,
        expected_sequence: u64,
        lines: i64,
    },
    /// Save only the owner-selected, already-associated visited file.
    SaveVisited {
        request_id: u64,
        max_bytes: usize,
    },
    /// Owned source only: no runtime Object, Context or gateway crosses threads.
    Evaluate {
        request_id: u64,
        source: String,
    },
    Command(rune_editor::Command),
    TextInsertion {
        window_id: u64,
        buffer_id: u64,
        text: String,
    },
    Composition(CompositionRequest),
}
/// Opaque scheduling marker. Only this module can construct one in safe Rust.
#[derive(Debug)]
pub struct RowWakeToken { _private: () }

#[derive(Clone, Debug)]
pub struct NativeKeyRequest {
    pub window_id: u64,
    pub buffer_id: u64,
    pub key: String,
    pub control: bool,
    pub meta: bool,
    pub shift: bool,
}
pub type NativeKeyDispatcher<H> =
    fn(&mut H, &NativeKeyRequest, Arc<std::sync::atomic::AtomicBool>) -> Result<bool, String>;
#[derive(Debug)]
pub struct CompositionAck {
    pub token: CompositionToken,
    pub outcome: Result<(), BridgeError>,
}
struct ReplyState {
    outstanding: bool,
    ack: Option<CompositionAck>,
    ui_alive: bool,
}
/// Single UI producer ownership. After attachment, submit through FrameView so
/// no cloned sender can overtake an accepted UI-local FIFO backlog.
pub struct NativeEditorSender {
    sender: Arc<SyncSender<OwnerEvent>>,
    row_control: Arc<RowControlState>,
    reply: Arc<Mutex<ReplyState>>,
}
pub struct NativeEditorReplies(Arc<Mutex<ReplyState>>);
#[must_use = "Bind inside the owner callback or explicitly reject_unbound"]
pub struct NativeEditorOwner {
    row_control: Arc<RowControlState>,
    receiver: Receiver<OwnerEvent>,
    reply: Arc<Mutex<ReplyState>>,
    publisher: HostPublisher,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisitedSaveKind {
    Saved,
    Unchanged,
}
#[derive(Debug)]
pub struct VisitedSave {
    pub kind: VisitedSaveKind,
    pub path: std::path::PathBuf,
    pub bytes: usize,
}
#[derive(Debug)]
pub struct SaveReply {
    pub request_id: u64,
    pub outcome: Result<VisitedSave, String>,
}
pub type VisitedSaver<H> = fn(&mut H, usize) -> Result<VisitedSave, String>;
pub type InterruptibleEvaluator<H> =
    fn(&mut H, &str, Arc<std::sync::atomic::AtomicBool>) -> Result<(), String>;
pub struct NativeEditorWorker<H: EditorHost>
where
    H::Doc: DisplayDocument,
{
    bridge: EditorBridge<H>,
    row_control: Arc<RowControlState>,
    receiver: Receiver<OwnerEvent>,
    reply: Arc<Mutex<ReplyState>>,
    publisher: HostPublisher,
    initialized: bool,
    visited_saver: Option<VisitedSaver<H>>,
    key_dispatcher: Option<(NativeKeyDispatcher<H>, crate::evaluation_quit::EvaluationQuit)>,
    evaluator: Option<fn(&mut H, &str) -> Result<(), String>>,
    interruptible: Option<(InterruptibleEvaluator<H>, crate::evaluation_quit::EvaluationQuit)>,
    fonts: Option<crate::font_transport::FontPlanPublisher>,
    platform: Option<crate::platform_context::PlatformPublisher>,
    prepare_input: Option<
        fn(
            &EditorBridge<H>,
            &FrameDisplay,
        ) -> Result<crate::platform_context::PlatformInputSnapshot, BridgeError>,
    >,
}
#[derive(Debug)]
pub enum InitializationError {
    Prepare(BridgeError),
    Publish(&'static str),
}
#[derive(Debug)]
pub struct InitializationFailure {
    pub error: InitializationError,
    pub undelivered: Vec<OwnerEvent>,
}
#[derive(Debug)]
pub struct OwnerStep {
    pub edit: Result<bool, BridgeError>,
    pub evaluation: Option<EvaluationOutcome>,
    pub save: Option<SaveReply>,
    pub key_outcome: Option<Result<(), String>>,
    pub render_error: Option<BridgeError>,
}
#[derive(Debug)]
pub struct EvaluationOutcome {
    pub request_id: u64,
    pub outcome: Result<(), String>,
}
fn close_and_drain(receiver: &Receiver<OwnerEvent>, reply: &Mutex<ReplyState>) -> Vec<OwnerEvent> {
    // Same lock as admission: previously admitted sends finish before closure,
    // and no racing late sender can enqueue after this bounded drain begins.
    reply.lock().expect("editor reply poisoned").ui_alive = false;
    receiver.try_iter().filter(|event| !matches!(event, OwnerEvent::RowWake(_))).collect()
}
struct RowControlState {
    enabled: AtomicBool,
    closed: AtomicBool,
    stop: AtomicBool,
    pending: AtomicBool,
    input_closed: AtomicBool,
    capacity: usize,
    #[cfg(test)]
    before_recv: Mutex<Option<SyncSender<()>>>,
    #[cfg(test)]
    service_pause: Mutex<Option<(SyncSender<()>, Arc<std::sync::Barrier>)>>,
}

/// Weak, activation-scoped scheduling/stop handle; never a general input producer.
/// Retaining it does not keep the legacy input channel connected. Stop is only
/// observed by run_with_row_activity (including a request made before activation).
#[derive(Clone)]
pub struct NativeRowController {
    sender: Weak<SyncSender<OwnerEvent>>,
    state: Arc<RowControlState>,
}
impl NativeRowController {
    pub fn request_stop(&self) {
        self.state.stop.store(true, Ordering::Release);
        if let Some(sender) = self.sender.upgrade() {
            schedule_row_wake(&sender, &self.state);
        }
    }
}

fn schedule_row_wake(sender: &SyncSender<OwnerEvent>, state: &RowControlState) {
    if !state.enabled.load(Ordering::Acquire) || state.closed.load(Ordering::Acquire) {
        return;
    }
    if state.pending.swap(true, Ordering::AcqRel) { return; }
    match sender.try_send(OwnerEvent::RowWake(RowWakeToken { _private: () })) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => { state.closed.store(true, Ordering::Release); }
    }
    // Full retains pending. Positive capacity guarantees a queued owner turn;
    // that turn services rows before blocking again. No payload is retried.
}
struct RowScheduler {
    sender: Arc<SyncSender<OwnerEvent>>,
    state: Arc<RowControlState>,
}
impl std::task::Wake for RowScheduler {
    fn wake(self: Arc<Self>) { schedule_row_wake(&self.sender, &self.state); }
    fn wake_by_ref(self: &Arc<Self>) { schedule_row_wake(&self.sender, &self.state); }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowOwnerExitReason { StopRequested, InputClosed, RowUiClosed }
#[derive(Debug)]
pub struct RowOwnerExit {
    pub reason: RowOwnerExitReason,
    pub undelivered: Vec<OwnerEvent>,
}

pub fn pending_editor_transport(
    capacity: usize,
    publisher: HostPublisher,
) -> (NativeEditorSender, NativeEditorReplies, NativeEditorOwner) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let sender = Arc::new(sender);
    let row_control = Arc::new(RowControlState {
        enabled: AtomicBool::new(false), closed: AtomicBool::new(false),
        stop: AtomicBool::new(false), pending: AtomicBool::new(false),
        input_closed: AtomicBool::new(false), capacity,
        #[cfg(test)]
        before_recv: Mutex::new(None),
        #[cfg(test)]
        service_pause: Mutex::new(None),
    });
    let reply = Arc::new(Mutex::new(ReplyState { outstanding: false, ack: None, ui_alive: true }));
    (
        NativeEditorSender { sender, row_control: row_control.clone(), reply: reply.clone() },
        NativeEditorReplies(reply.clone()),
        NativeEditorOwner { row_control, receiver, reply, publisher },
    )
}
impl NativeEditorOwner {
    pub fn reject_unbound(self) -> Vec<OwnerEvent> {
        self.row_control.closed.store(true, Ordering::Release);
        close_and_drain(&self.receiver, &self.reply)
    }
    /// H is deliberately not Send/static. Construct it inside the runtime owner
    /// callback, never move a live Context to a UI or background GPUI executor.
    pub fn bind<H: EditorHost>(self, bridge: EditorBridge<H>) -> NativeEditorWorker<H>
    where
        H::Doc: DisplayDocument,
    {
        NativeEditorWorker {
            bridge,
            row_control: self.row_control,
            receiver: self.receiver,
            reply: self.reply,
            publisher: self.publisher,
            initialized: false,
            visited_saver: None,
            key_dispatcher: None,
            evaluator: None,
            interruptible: None,
            fonts: None,
            platform: None,
            prepare_input: None,
        }
    }
    pub fn bind_ime_initialized<H: EditorHost>(
        self,
        bridge: EditorBridge<H>,
        platform: crate::platform_context::PlatformPublisher,
    ) -> Result<NativeEditorWorker<H>, InitializationFailure>
    where
        H::Doc: rune_editor::PlatformTextDocument,
    {
        if bridge.has_font_projection(){return Err(InitializationFailure{error:InitializationError::Prepare(BridgeError::Publish("font mode is not enabled for platform IME")),undelivered:self.reject_unbound()})}
        let bridge = match bridge.with_text_source(crate::editor_bridge::TextSource::CommittedText)
        {
            Ok(bridge) => bridge,
            Err(error) => {
                return Err(InitializationFailure {
                    error: InitializationError::Prepare(error),
                    undelivered: self.reject_unbound(),
                });
            }
        };
        let mut worker = self.bind(bridge);
        worker.platform = Some(platform);
        worker.prepare_input = Some(crate::platform_context::prepare::<H>);
        worker.initialize()
    }
    /// Explicit non-IME font mode. Configure before first publication so startup
    /// failures retain the same definitely-undelivered event accounting.
    pub fn bind_font_initialized<H:EditorHost>(self,bridge:EditorBridge<H>,capabilities:rune_display::MeasuredFontCapabilities,project:crate::font_transport::FontProjector<H>,publisher:crate::font_transport::FontPlanPublisher)->Result<NativeEditorWorker<H>,InitializationFailure>
    where H::Doc:DisplayDocument {
        let bridge=match bridge.with_font_projector(capabilities,project){Ok(b)=>b,Err(error)=>return Err(InitializationFailure{error:InitializationError::Prepare(error),undelivered:self.reject_unbound()})};
        let mut worker=self.bind(bridge);worker.fonts=Some(publisher);worker.initialize()
    }
    pub fn bind_initialized<H: EditorHost>(
        self,
        bridge: EditorBridge<H>,
    ) -> Result<NativeEditorWorker<H>, InitializationFailure>
    where
        H::Doc: DisplayDocument,
    {
        self.bind(bridge).initialize()
    }
}
pub fn editor_transport<H: EditorHost>(
    bridge: EditorBridge<H>,
    capacity: usize,
    publisher: HostPublisher,
) -> (NativeEditorSender, NativeEditorReplies, NativeEditorWorker<H>)
where
    H::Doc: DisplayDocument,
{
    let (sender, replies, owner) = pending_editor_transport(capacity, publisher);
    (sender, replies, owner.bind(bridge))
}
impl NativeEditorSender {
    /// A narrow scheduling/stop handle, never a second general input producer.
    pub fn row_lifecycle_controller(&self) -> NativeRowController {
        NativeRowController { sender: Arc::downgrade(&self.sender), state: self.row_control.clone() }
    }
    /// Retry only returned ownership. An enqueued request must never be resent
    /// because an ack is late; one reserved slot retains its exact edit outcome.
    pub fn try_send(&self, event: OwnerEvent) -> Result<(), TrySendError<OwnerEvent>> {
        let mut r = self.reply.lock().expect("editor reply poisoned");
        if !r.ui_alive {
            return Err(TrySendError::Disconnected(event));
        }
        let composition = matches!(&event, OwnerEvent::Composition(_));
        if composition && r.outstanding {
            return Err(TrySendError::Full(event));
        }
        self.sender.try_send(event)?;
        if composition {
            r.outstanding = true;
        }
        Ok(())
    }
}
impl Drop for NativeEditorSender {
    fn drop(&mut self) {
        self.row_control.input_closed.store(true, Ordering::Release);
        schedule_row_wake(&self.sender, &self.row_control);
    }
}

impl NativeEditorReplies {
    /// Check after explicit HostUpdates wake, not on an idle polling timer.
    pub fn take(&mut self) -> Option<CompositionAck> {
        let mut r = self.0.lock().ok()?;
        let ack = r.ack.take()?;
        r.outstanding = false;
        Some(ack)
    }
}
impl Drop for NativeEditorReplies {
    fn drop(&mut self) {
        if let Ok(mut r) = self.0.lock() {
            r.ui_alive = false;
        }
    }
}
impl<H: EditorHost> Drop for NativeEditorWorker<H>
where H::Doc: DisplayDocument {
    fn drop(&mut self) { self.row_control.closed.store(true, Ordering::Release); }
}

impl<H: EditorHost> NativeEditorWorker<H>
where
    H::Doc: DisplayDocument,
{
    pub fn with_key_dispatcher(
        mut self,
        dispatch: NativeKeyDispatcher<H>,
        quit: crate::evaluation_quit::EvaluationQuit,
    ) -> Self {
        self.key_dispatcher = Some((dispatch, quit));
        self
    }
    pub fn bridge(&self) -> &EditorBridge<H> {
        &self.bridge
    }
    /// Raw dependency-light step. Production run enforces initial publication.
    pub fn try_step(&mut self) -> Result<OwnerStep, TryRecvError> {
        let e = self.receiver.try_recv()?;
        Ok(self.process(e))
    }
    pub fn initialize(mut self) -> Result<Self, InitializationFailure> {
        if self.initialized {
            return Ok(self);
        }
        let result = self.retry_frame().map_err(|error| match error {
            BridgeError::Publish(message) => InitializationError::Publish(message),
            error => InitializationError::Prepare(error),
        });
        if let Err(error) = result {
            return Err(InitializationFailure {
                error,
                undelivered: close_and_drain(&self.receiver, &self.reply),
            });
        }
        self.initialized = true;
        Ok(self)
    }
    /// Dedicated std::thread only. Initial errors/undelivered inputs must reach
    /// caller's UI failure path; no success ack is manufactured on setup failure.
    pub fn run(self) -> Result<(), InitializationFailure> {
        self.run_with_outcome(|step| {
            if let Some(save) = step.save {
                if let Err(error) = save.outcome {
                    eprintln!("native save {} failed: {error}", save.request_id);
                }
            }

            if let Some(evaluation) = step.evaluation {
                if let Err(error) = evaluation.outcome {
                    eprintln!("native evaluation {} failed: {error}", evaluation.request_id);
                }
            }
            if let Err(e) = step.edit {
                eprintln!("native owner input rejected: {e:?}");
            }
            if let Some(e) = step.render_error {
                eprintln!("native layout after edit failed: {e:?}");
            }
        })
    }
    /// Report every completed event once on the owner thread, including ordinary
    /// command/text failures that have no composition ACK. A root application can
    /// publish these plain outcomes to its visible status UI instead of relying
    /// on stderr. The callback runs after reply locks are dropped; it must not
    /// block the owner or replay an event after edit/render failure.
    /// Initialization failure still returns its error and undelivered events.
    pub fn run_with_outcome(
        self,
        mut report: impl FnMut(OwnerStep),
    ) -> Result<(), InitializationFailure> {
        let mut worker = self.initialize()?;
        while let Ok(event) = worker.receiver.recv() {
            report(worker.process(event));
        }
        Ok(())
    }
    /// Opt-in lifecycle-only runner. H remains on this same owner thread.
    /// Service receives a readonly bridge, never mutable host/evaluator authority.
    /// It must be bounded/nonblocking and must not replay edits on row errors.
    pub fn run_with_row_activity(
        self,
        mut rows: rune_display::retained_row_handoff::RowPreparationOwner,
        controller: &NativeRowController,
        service: fn(&EditorBridge<H>, &mut rune_display::retained_row_handoff::RowPreparationOwner)
            -> Result<(), String>,
        mut report: impl FnMut(OwnerStep),
        mut row_report: impl FnMut(Result<(), String>),
    ) -> Result<RowOwnerExit, InitializationFailure> {
        if !Arc::ptr_eq(&self.row_control, &controller.state) || self.row_control.capacity == 0 {
            self.row_control.closed.store(true, Ordering::Release);
            return Err(InitializationFailure {
                error: InitializationError::Publish("row runner requires its own positive-capacity controller"),
                undelivered: close_and_drain(&self.receiver, &self.reply),
            });
        }
        self.row_control.enabled.store(true, Ordering::Release);
        let mut worker = self.initialize()?;
        let Some(sender) = controller.sender.upgrade() else {
            return Ok(worker.close_row_runner(&mut rows, RowOwnerExitReason::InputClosed));
        };
        let waker = std::task::Waker::from(Arc::new(RowScheduler {
            sender, state: worker.row_control.clone(),
        }));
        loop {
            if let Some(reason) = worker.row_stop_reason() {
                return Ok(worker.close_row_runner(&mut rows, reason));
            }
            // Clear BEFORE callback/drain: activity during service schedules anew.
            worker.row_control.pending.store(false, Ordering::Release);
            #[cfg(test)]
            {
                let pause = worker.row_control.service_pause.lock().unwrap().take();
                if let Some((entered, resume)) = pause {
                    let _ = entered.try_send(());
                    resume.wait();
                }
            }
            row_report(service(&worker.bridge, &mut rows));
            if let Some(reason) = worker.row_stop_reason() {
                return Ok(worker.close_row_runner(&mut rows, reason));
            }
            // Check row closure before starting another data turn, while giving
            // queued FIFO data priority over nonterminal row-ready edges.
            enum Received { RowReady, RowClosed, InputClosed, Event(OwnerEvent) }
            let received = {
                use std::future::Future;
                let mut wait = std::pin::pin!(rows.wait_for_activity());
                let activity = wait.as_mut().poll(&mut std::task::Context::from_waker(&waker));
                if matches!(activity, std::task::Poll::Ready(
                    rune_display::retained_row_handoff::RowActivity::Disconnected))
                {
                    Received::RowClosed
                } else {
                    match worker.receiver.try_recv() {
                        Ok(event) => Received::Event(event),
                        Err(TryRecvError::Disconnected) => Received::InputClosed,
                        Err(TryRecvError::Empty) if activity.is_ready() => Received::RowReady,
                        Err(TryRecvError::Empty) => {
                            #[cfg(test)]
                            if let Some(notice) = worker.row_control.before_recv.lock().unwrap().as_ref() {
                                let _ = notice.try_send(());
                            }
                            match worker.receiver.recv() {
                                Ok(event) => Received::Event(event),
                                Err(_) => Received::InputClosed,
                            }
                        }
                    }
                }
            }; // Wait unregisters before processing/closing; no host borrow crosses recv.
            let event = match received {
                Received::RowReady => continue,
                Received::RowClosed => return Ok(worker.close_row_runner(&mut rows, RowOwnerExitReason::RowUiClosed)),
                Received::InputClosed => return Ok(worker.close_row_runner(&mut rows, RowOwnerExitReason::InputClosed)),
                Received::Event(event) => event,
            };
            if matches!(event, OwnerEvent::RowWake(_)) {
                let _ = worker.publisher.input_capacity_available();
            } else {
                report(worker.process(event));
            }
        }
    }

    fn row_stop_reason(&self) -> Option<RowOwnerExitReason> {
        if self.row_control.stop.load(Ordering::Acquire) {
            Some(RowOwnerExitReason::StopRequested)
        } else if self.row_control.input_closed.load(Ordering::Acquire) {
            Some(RowOwnerExitReason::InputClosed)
        } else { None }
    }

    fn close_row_runner(
        &self,
        rows: &mut rune_display::retained_row_handoff::RowPreparationOwner,
        reason: RowOwnerExitReason,
    ) -> RowOwnerExit {
        self.row_control.closed.store(true, Ordering::Release);
        rows.close();
        let undelivered = close_and_drain(&self.receiver, &self.reply).into_iter()
            .filter(|event| !matches!(event, OwnerEvent::RowWake(_))).collect();
        RowOwnerExit { reason, undelivered }
    }

    /// Install an owner-only evaluator. Any persistent rooted evaluator state
    /// belongs in H, not a UI closure. Absent adapters explicitly reject source.
    pub fn with_evaluator(mut self, evaluate: fn(&mut H, &str) -> Result<(), String>) -> Self {
        self.evaluator = Some(evaluate);
        self.interruptible = None;
        self
    }
    /// Configure a scoped interruption token, active only during one callback.
    /// Idle C-g is not carried forward; accepted evaluations are never replayed.
    pub fn with_interruptible_evaluator(
        mut self,
        evaluate: InterruptibleEvaluator<H>,
        signal: crate::evaluation_quit::EvaluationQuit,
    ) -> Self {
        self.evaluator = None;
        self.interruptible = Some((evaluate, signal));
        self
    }
    /// Owner-only selected visited-file callback. Never replay a save because
    /// its reply or subsequent publication failed; path authority stays in H.
    pub fn with_visited_saver(mut self, save: VisitedSaver<H>) -> Self {
        self.visited_saver = Some(save);
        self
    }
    /// Retry preparation/publication only. Never replay a committed command or
    /// composition request to recover a later rendering failure.
    pub fn retry_frame(&mut self) -> Result<(), BridgeError> {
        let frame = self.bridge.prepare_frame()?;
        if let (Some(platform), Some(prepare)) = (&self.platform, self.prepare_input) {
            let input = prepare(&self.bridge, &frame)?;
            platform.publish(frame.clone(), input)?;
        }
        if let Some(fonts)=&self.fonts {
            let plan=self.bridge.prepared_font_plan().ok_or(BridgeError::Publish("missing prepared font plan"))?;
            if !plan.matches_frame(&frame){return Err(BridgeError::Publish("font plan/frame identity mismatch"))}
            fonts.publish(plan).map_err(BridgeError::Publish)?;
        }
        self.publisher.publish(frame).map_err(BridgeError::Publish)
    }
    fn process(&mut self, event: OwnerEvent) -> OwnerStep {
        let mut evaluation = None;
        let mut key_outcome = None;
        let mut force_frame = false;
        let mut save = None;
        let mut preserve_context = false;
        let edit = match event {
            OwnerEvent::RowWake(_) => Ok(false),
            OwnerEvent::SaveVisited { request_id, max_bytes } => {
                match self.bridge.owner_publication_revision() {
                    Err(error) => {
                        save = Some(SaveReply {
                            request_id,
                            outcome: Err(format!("Save not started: {error:?}")),
                        });
                        Err(error)
                    }
                    Ok(before) => {
                        let outcome = if let Some(callback) = self.visited_saver {
                            self.bridge.save_with(callback, max_bytes)
                        } else {
                            Err("no visited-file saver configured".into())
                        };
                        let success = outcome.is_ok();
                        save = Some(SaveReply { request_id, outcome });
                        match self.bridge.owner_publication_revision() {
                            Ok(after) => {
                                let changed = before != after;
                                preserve_context = before.0 == after.0;
                                Ok(success || changed)
                            }
                            Err(error) => {
                                if let Some(platform) = &self.platform {
                                    let _ = platform.invalidate();
                                }
                                Err(error)
                            }
                        }
                    }
                }
            }
            OwnerEvent::Evaluate { request_id, source } => {
                let before = self.bridge.owner_publication_revision();
                match before {
                    Err(error) => {
                        evaluation = Some(EvaluationOutcome {
                            request_id,
                            outcome: Err(format!("owner snapshot before evaluation: {error:?}")),
                        });
                        Err(error)
                    }
                    Ok(before) => {
                        let outcome = if source.len() > 65_536 {
                            Err("evaluation source exceeds 65536-byte limit".into())
                        } else if let Some((evaluate, signal)) = &self.interruptible {
                            match signal.begin() {
                                Ok(scope) => self.bridge.evaluate_with_quit(
                                    *evaluate,
                                    &source,
                                    scope.token(),
                                ),
                                Err(error) => Err(error.into()),
                            }
                        } else if let Some(evaluate) = self.evaluator {
                            self.bridge.evaluate_with(evaluate, &source)
                        } else {
                            Err("no owner evaluator configured".into())
                        };
                        evaluation = Some(EvaluationOutcome { request_id, outcome });
                        // Evaluation may edit and then signal. Never replay it;
                        // publication follows owner sequence, not its Result.
                        match self.bridge.owner_publication_revision() {
                            Ok(after) => { preserve_context = before.0 == after.0; Ok(after != before) },
                            Err(error) => {
                                if let Some(platform) = &self.platform {
                                    let _ = platform.invalidate();
                                }
                                Err(error)
                            }
                        }
                    }
                }
            }
            OwnerEvent::NativeKey { request, fallback } => {
                force_frame = true;
                if let Some((dispatch, channel)) = &self.key_dispatcher {
                    let dispatch = *dispatch;
                    let channel = channel.clone();
                    let before = if request.key.len()>128 {Err(BridgeError::UnsupportedInput)} else {self.bridge.owner_publication_revision()};
                    match before {
                        Err(error) => Err(error),
                        Ok(before) => {
                            let outcome = {
                                match channel.begin() {
                                    Ok(scope) => self.bridge.dispatch_key_with(
                                        dispatch,
                                        &request,
                                        scope.token(),
                                    ),
                                    Err(error) => Err(error.into()),
                                }
                            };
                            let handled = outcome.as_ref().copied().unwrap_or(true);
                            key_outcome = Some(outcome.map(|_| ()));
                            if !handled {
                                if let Some(command) = fallback {
                                    self.bridge.execute_command(command)
                                } else {
                                    Err(BridgeError::UnsupportedInput)
                                }
                            } else {
                                self.bridge.owner_publication_revision().map(|s| { preserve_context = s.0 == before.0; s != before })
                            }
                        }
                    }
                } else if let Some(command) = fallback {
                    self.bridge.execute_command(command)
                } else {
                    Err(BridgeError::UnsupportedInput)
                }
            }
            OwnerEvent::KeyText { window_id, buffer_id, text } => {
                force_frame = true;
                if let Some((dispatch, channel)) = &self.key_dispatcher {
                    let dispatch = *dispatch;
                    let channel = channel.clone();
                    let before = self.bridge.owner_publication_revision();
                    match before {
                        Err(error) => Err(error),
                        Ok(before) => {
                            let request = NativeKeyRequest {
                                window_id,
                                buffer_id,
                                key: text.clone(),
                                control: false,
                                meta: false,
                                shift: false,
                            };
                            let outcome = if text.chars().count() != 1 {
                                Err("key text must contain one scalar".into())
                            } else {
                                match channel.begin() {
                                    Ok(scope) => self.bridge.dispatch_key_with(
                                        dispatch,
                                        &request,
                                        scope.token(),
                                    ),
                                    Err(error) => Err(error.into()),
                                }
                            };
                            let handled = outcome.as_ref().copied().unwrap_or(true);
                            key_outcome = Some(outcome.map(|_| ()));
                            if !handled {
                                self.bridge.commit_text(window_id, buffer_id, text)
                            } else {
                                self.bridge.owner_publication_revision().map(|s| { preserve_context = s.0 == before.0; s != before })
                            }
                        }
                    }
                } else {
                    self.bridge.commit_text(window_id, buffer_id, text)
                }
            }
            OwnerEvent::Input(e) => {
                if let rune_display::InputEvent::Key { key, control, meta, shift, .. } = &e {
                    if (*control && !*meta && key == "g")
                        || (!*control && !*meta && key == "escape")
                    {
                        if let Some((dispatch, _)) = &self.key_dispatcher {
                            let request = NativeKeyRequest {
                                window_id: 0,
                                buffer_id: 0,
                                key: key.clone(),
                                control: *control,
                                meta: *meta,
                                shift: *shift,
                            };
                            key_outcome = Some(
                                self.bridge
                                    .dispatch_key_with(
                                        *dispatch,
                                        &request,
                                        Arc::new(std::sync::atomic::AtomicBool::new(false)),
                                    )
                                    .map(|_| ()),
                            );
                        }
                    }
                }
                self.bridge.apply(&e)
            }
            OwnerEvent::ScrollWindow { window_id, buffer_id, expected_sequence, lines } => {
                self.bridge.scroll_window(window_id, buffer_id, expected_sequence, lines)
            }
            OwnerEvent::Command(command) => self.bridge.execute_command(command),
            OwnerEvent::TextInsertion { window_id, buffer_id, text } => {
                self.bridge.commit_text(window_id, buffer_id, text)
            }
            OwnerEvent::Composition(request) => {
                let edit = self.bridge.commit_composition(request.committed);
                let outcome = edit.as_ref().map(|_| ()).map_err(Clone::clone);
                let mut r = self.reply.lock().expect("editor reply poisoned");
                assert!(r.ack.is_none());
                r.ack = Some(CompositionAck { token: request.token, outcome });
                edit
            }
        };
        // Invalidate context before wake when an edit/point transition happened.
        // A failed subsequent preparation cannot leave an old writable IME scope.
        let mut render_error = None;
        if matches!(edit, Ok(true)) && !preserve_context {
            if let Some(platform) = &self.platform {
                if let Err(error) = platform.invalidate() {
                    render_error = Some(error);
                }
            }
        }
        let _ = self.publisher.input_capacity_available();
        if matches!(edit, Ok(true)) || force_frame {
            if let Err(error) = self.retry_frame() {
                render_error = Some(error);
            }
        }
        OwnerStep { edit, evaluation, save, key_outcome, render_error }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        composition::{CompositionContext, CompositionState},
        editor_bridge::TextSource,
    };
    use rune_display::{FixedMetrics, Rect, TextSnapshot, host_updates};
    use rune_editor::{Document, RopeDocument, Session};
    fn bridge() -> EditorBridge<Session<RopeDocument>> {
        EditorBridge::new(
            Session::new(RopeDocument::empty(), 80, 24).unwrap(),
            FixedMetrics::default(),
            Rect { x: 0., y: 0., width: 640., height: 480. },
        )
        .unwrap()
        .with_text_source(TextSource::CommittedText)
        .unwrap()
    }
    fn request(b: &EditorBridge<Session<RopeDocument>>) -> CompositionRequest {
        let s = b.snapshot().unwrap();
        let v = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        let b = s.buffers.iter().find(|b| b.id == v.view.buffer).unwrap();
        let mut c = CompositionState::new().unwrap();
        c.set_context(Some(CompositionContext {
            window_id: v.id.as_u64(),
            buffer_id: b.id.as_u64(),
            publication_epoch: b.display_snapshot().revision(),
            owner_sequence: s.sequence,
            replacement: v.view.point..v.view.point,
        }))
        .unwrap();
        let token = c.begin().unwrap();
        CompositionRequest { token, committed: c.commit(token, "λ🙂".into()).unwrap() }
    }
    #[test]
    fn commit_ack_is_once_and_blocks_second_composition_until_taken() {
        let b = bridge();
        let first = request(&b);
        let second = request(&b);
        let token = first.token;
        let (p, _u) = host_updates();
        let (s, mut r, mut w) = editor_transport(b, 2, p);
        s.try_send(OwnerEvent::Composition(first)).unwrap();
        w.try_step().unwrap().edit.unwrap();
        assert!(matches!(
            s.try_send(OwnerEvent::Composition(second)),
            Err(TrySendError::Full(_))
        ));
        let ack = r.take().unwrap();
        assert_eq!(ack.token, token);
        assert!(ack.outcome.is_ok());
        assert!(r.take().is_none());
        assert_eq!(
            {
                let s = w.bridge().snapshot().unwrap();
                s.views.iter().find(|v| v.id == s.selected_window).unwrap().view.point
            },
            2
        );
        assert!(matches!(w.try_step(), Err(TryRecvError::Empty)));
    }
    #[test]
    fn rejected_unbound_owner_returns_identity_and_closes_admission() {
        let b = bridge();
        let req = request(&b);
        let token = req.token;
        let (p, _u) = host_updates();
        let (s, mut r, o) = pending_editor_transport(1, p);
        s.try_send(OwnerEvent::Composition(req)).unwrap();
        assert!(
            matches!(o.reject_unbound().as_slice(),[OwnerEvent::Composition(req)] if req.token==token)
        );
        assert!(matches!(
            s.try_send(OwnerEvent::Input(InputEvent::TextCommit("late".into()))),
            Err(TrySendError::Disconnected(_))
        ));
        assert!(r.take().is_none());
    }
    #[test]
    fn failed_initial_publication_returns_original_unexecuted_input() {
        let b = bridge();
        let req = request(&b);
        let token = req.token;
        let (p, u) = host_updates();
        let (s, mut r, o) = pending_editor_transport(1, p);
        s.try_send(OwnerEvent::Composition(req)).unwrap();
        drop(u);
        let e = match o.bind_initialized(b) {
            Err(e) => e,
            Ok(_) => panic!("publication should fail"),
        };
        assert!(matches!(e.error, InitializationError::Publish(_)));
        assert!(
            matches!(e.undelivered.as_slice(),[OwnerEvent::Composition(req)] if req.token==token)
        );
        assert!(r.take().is_none());
    }
    #[test]
    fn edit_success_ack_survives_publication_failure_and_frame_retry_never_reinserts() {
        let b = bridge();
        let req = request(&b);
        let (p, u) = host_updates();
        let (s, mut replies, mut worker) = editor_transport(b, 1, p);
        drop(u);
        s.try_send(OwnerEvent::Composition(req)).unwrap();
        let step = worker.try_step().unwrap();
        assert!(step.edit.unwrap());
        assert!(matches!(step.render_error, Some(BridgeError::Publish(_))));
        assert!(replies.take().unwrap().outcome.is_ok());
        assert!(matches!(worker.retry_frame(), Err(BridgeError::Publish(_))));
        assert!(matches!(worker.retry_frame(), Err(BridgeError::Publish(_))));
        assert_eq!(
            {
                let s = worker.bridge().snapshot().unwrap();
                s.views.iter().find(|v| v.id == s.selected_window).unwrap().view.point
            },
            2
        );
        assert!(replies.take().is_none());
        assert!(matches!(worker.try_step(), Err(TryRecvError::Empty)));
    }
    #[test]
    fn ordinary_platform_text_queues_without_composition_ack_retargeting_or_duplicate_insert() {
        let b = bridge();
        let snapshot = b.snapshot().unwrap();
        let window_id = snapshot.selected_window.as_u64();
        let buffer_id = snapshot.current_buffer.as_u64();
        let (p, _u) = host_updates();
        let (s, mut replies, mut worker) = editor_transport(b, 3, p);
        for text in ["a", "🙂", "b"] {
            s.try_send(OwnerEvent::TextInsertion { window_id, buffer_id, text: text.into() })
                .unwrap();
        }
        for _ in 0..3 {
            worker.try_step().unwrap().edit.unwrap();
        }
        let snapshot = worker.bridge().snapshot().unwrap();
        let doc = &snapshot
            .buffers
            .iter()
            .find(|b| b.id == snapshot.current_buffer)
            .unwrap()
            .document;
        assert_eq!(doc.to_text(), "a🙂b");
        assert!(replies.take().is_none());
    }
    fn edit_then_signal(host: &mut Session<RopeDocument>, source: &str) -> Result<(), String> {
        host.execute(rune_editor::Command::Insert(source.into()))
            .map_err(|e| e.to_string())?;
        Err("evaluation signalled after edit".into())
    }
    #[test]
    fn evaluation_error_after_edit_publishes_and_never_replays_on_frame_failure() {
        let (p, updates) = host_updates();
        let (sender, _replies, worker) = editor_transport(bridge(), 2, p);
        let mut worker = worker.with_evaluator(edit_then_signal);
        drop(updates);
        sender
            .try_send(OwnerEvent::Evaluate { request_id: 41, source: "λ".into() })
            .unwrap();
        let step = worker.try_step().unwrap();
        assert_eq!(step.edit.unwrap(), true);
        let outcome = step.evaluation.unwrap();
        assert_eq!(outcome.request_id, 41);
        assert_eq!(outcome.outcome.unwrap_err(), "evaluation signalled after edit");
        assert!(matches!(step.render_error, Some(BridgeError::Publish(_))));
        assert!(worker.retry_frame().is_err());
        let snapshot = worker.bridge().snapshot().unwrap();
        assert_eq!(snapshot.buffers[0].document.to_text(), "λ");
        sender.try_send(OwnerEvent::Command(rune_editor::Command::Undo)).unwrap();
        worker.try_step().unwrap().edit.unwrap();
        assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "");
    }
    #[test]
    fn owner_outcome_reports_unconfigured_evaluation_once_without_edit() {
        let (p, _updates) = host_updates();
        let (sender, _replies, worker) = editor_transport(bridge(), 2, p);
        sender
            .try_send(OwnerEvent::Evaluate { request_id: 7, source: "ignored".into() })
            .unwrap();
        drop(sender);
        let mut seen = 0;
        worker
            .run_with_outcome(|step| {
                seen += 1;
                assert!(!step.edit.unwrap());
                assert!(step.render_error.is_none());
                let evaluation = step.evaluation.unwrap();
                assert_eq!(evaluation.request_id, 7);
                assert_eq!(evaluation.outcome.unwrap_err(), "no owner evaluator configured");
            })
            .unwrap();
        assert_eq!(seen, 1);
    }
    #[test]
    fn evaluation_error_edit_precedes_and_stales_captured_composition_in_same_fifo() {
        let b = bridge();
        let captured = request(&b);
        let (publisher, _updates) = host_updates();
        let (sender, mut replies, worker) = editor_transport(b, 2, publisher);
        let mut worker = worker.with_evaluator(edit_then_signal);
        sender
            .try_send(OwnerEvent::Evaluate { request_id: 9, source: "before".into() })
            .unwrap();
        sender.try_send(OwnerEvent::Composition(captured)).unwrap();
        let evaluation = worker.try_step().unwrap();
        assert!(evaluation.edit.unwrap());
        assert!(evaluation.evaluation.unwrap().outcome.is_err());
        let composition = worker.try_step().unwrap();
        assert!(matches!(
            composition.edit,
            Err(BridgeError::Editor(rune_editor::EditorError::StaleInput))
        ));
        assert!(replies.take().unwrap().outcome.is_err());
        assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "before");
    }
    thread_local! {
        static TEST_SIGNAL: std::cell::RefCell<Option<crate::evaluation_quit::EvaluationQuit>> = const {std::cell::RefCell::new(None)};
    }
    fn scoped_interruption(
        host: &mut Session<RopeDocument>,
        source: &str,
        token: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        assert!(!token.load(Ordering::Acquire));
        TEST_SIGNAL.with(|signal| assert!(signal.borrow().as_ref().unwrap().request()));
        assert!(token.swap(false, Ordering::AcqRel));
        if source == "edit" {
            host.execute(rune_editor::Command::Insert("partial".into()))
                .map_err(|e| e.to_string())?;
        }
        Err("mock structured interruption".into())
    }
    #[test]
    fn scoped_callback_closes_signal_after_error_and_next_request_starts_fresh() {
        let signal = crate::evaluation_quit::EvaluationQuit::default();
        TEST_SIGNAL.with(|slot| *slot.borrow_mut() = Some(signal.clone()));
        let (publisher, _updates) = host_updates();
        let (sender, _replies, worker) = editor_transport(bridge(), 2, publisher);
        let mut worker = worker.with_interruptible_evaluator(scoped_interruption, signal.clone());
        assert!(!signal.request());
        for (request_id, source) in [(1, "edit"), (2, "no edit")] {
            sender
                .try_send(OwnerEvent::Evaluate { request_id, source: source.into() })
                .unwrap();
            let step = worker.try_step().unwrap();
            assert_eq!(step.edit.unwrap(), request_id == 1);
            assert_eq!(
                step.evaluation.unwrap().outcome.unwrap_err(),
                "mock structured interruption"
            );
            assert!(!signal.request());
        }
        assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "partial");
        TEST_SIGNAL.with(|slot| *slot.borrow_mut() = None);
    }
    thread_local! { static SAVE_CALLS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)}; }
    fn counted_save(host: &mut Session<RopeDocument>, limit: usize) -> Result<VisitedSave, String> {
        SAVE_CALLS.with(|calls| calls.set(calls.get() + 1));
        let current = host.current_buffer();
        let bytes = host.document(current).map_err(|e| e.to_string())?.to_text().len();
        if bytes > limit {
            return Err("save byte limit".into());
        }
        host.mark_saved(current).map_err(|e| e.to_string())?;
        Ok(VisitedSave { kind: VisitedSaveKind::Saved, path: "/owner/fixed-path".into(), bytes })
    }
    #[test]
    fn save_outcome_survives_frame_failure_and_retry_never_replays_callback() {
        SAVE_CALLS.with(|calls| calls.set(0));
        let (publisher, updates) = host_updates();
        let (sender, _replies, worker) = editor_transport(bridge(), 4, publisher);
        let mut worker = worker.initialize().unwrap().with_visited_saver(counted_save);
        drop(updates);
        sender
            .try_send(OwnerEvent::Command(rune_editor::Command::Insert("save me".into())))
            .unwrap();
        worker.try_step().unwrap().edit.unwrap();
        sender
            .try_send(OwnerEvent::SaveVisited { request_id: 3, max_bytes: 64 })
            .unwrap();
        let step = worker.try_step().unwrap();
        assert!(step.edit.unwrap());
        assert!(step.render_error.is_some());
        let reply = step.save.unwrap();
        assert_eq!(reply.request_id, 3);
        assert_eq!(reply.outcome.unwrap().bytes, 7);
        assert!(worker.retry_frame().is_err());
        assert!(worker.retry_frame().is_err());
        SAVE_CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert!(!worker.bridge().snapshot().unwrap().buffers[0].modified);
        sender
            .try_send(OwnerEvent::Command(rune_editor::Command::Insert("!".into())))
            .unwrap();
        worker.try_step().unwrap().edit.unwrap();
        assert!(worker.bridge().snapshot().unwrap().buffers[0].modified);
    }
    #[test]
    fn unconfigured_save_reports_explicit_failure_without_mutation() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, mut worker) = editor_transport(bridge(), 1, publisher);
        sender
            .try_send(OwnerEvent::SaveVisited { request_id: 9, max_bytes: 64 })
            .unwrap();
        let step = worker.try_step().unwrap();
        assert!(!step.edit.unwrap());
        assert!(step.render_error.is_none());
        let reply = step.save.unwrap();
        assert_eq!(reply.request_id, 9);
        assert_eq!(reply.outcome.unwrap_err(), "no visited-file saver configured");
    }
}

#[cfg(test)]
mod row_fifo_tests {
    use super::*;
    use crate::editor_bridge::TextSource;
    use rune_display::{FixedMetrics, Rect, TextSnapshot, host_updates};
    use rune_display::retained_row_handoff::{RowPreparationOwner, RowPreparationUi, RowActivity, RetainedRowReply, row_preparation_handoff};
    use rune_display::retained_row_transaction::RetainedRowScope;
    use rune_editor::{Document, RopeDocument, Session};
    use std::{future::Future, task::{Poll, Wake, Waker}, time::{Duration, Instant}};

    fn bridge() -> EditorBridge<Session<RopeDocument>> {
        EditorBridge::new(Session::new(RopeDocument::empty(), 80, 24).unwrap(),
            FixedMetrics::default(), Rect { x: 0., y: 0., width: 640., height: 480. })
            .unwrap().with_text_source(TextSource::CommittedText).unwrap()
    }
    fn scope_and_text(bridge: &EditorBridge<Session<RopeDocument>>) -> (RetainedRowScope, String) {
        let snapshot = bridge.snapshot().unwrap();
        let view = snapshot.views.iter().find(|view| view.id == snapshot.selected_window).unwrap();
        let buffer = snapshot.buffers.iter().find(|buffer| buffer.id == view.view.buffer).unwrap();
        let source = buffer.display_snapshot();
        (RetainedRowScope {
            sequence: snapshot.sequence, window: view.id.as_u64(), buffer: buffer.id.as_u64(),
            text_epoch: source.revision(), source_start_scalar: 0,
            style_revision: 1, viewport_revision: 1, font_bank_generation: 1, shaping_policy_revision: 1,
        }, source.chars_from(0).collect())
    }
    fn service(bridge: &EditorBridge<Session<RopeDocument>>, rows: &mut RowPreparationOwner) -> Result<(), String> {
        let (scope, text) = scope_and_text(bridge);
        rows.observe_owner_state(scope, &text).map_err(|error| format!("observe={error:?}"))?;
        rows.poll(scope, &text).map_err(|error| format!("poll={error:?}"))?;
        Ok(())
    }
    fn snapshot_error(bridge: &EditorBridge<Session<RopeDocument>>, _: &mut RowPreparationOwner) -> Result<(), String> {
        Err(format!("snapshot:{}", scope_and_text(bridge).1))
    }
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark { fn wake(self: Arc<Self>) { self.0.unpark(); } }
    fn wait_ui(ui: &mut RowPreparationUi, deadline: Instant) {
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut future = std::pin::pin!(ui.wait_for_activity());
        loop {
            let remaining = deadline.checked_duration_since(Instant::now()).expect("UI deadline");
            match future.as_mut().poll(&mut std::task::Context::from_waker(&waker)) {
                Poll::Ready(RowActivity::Ready) => return,
                Poll::Ready(RowActivity::Disconnected) => panic!("unexpected row disconnect"),
                Poll::Pending => std::thread::park_timeout(remaining),
            }
        }
    }

    #[test]
    fn row_fifo_minted_controller_does_not_keep_legacy_runner_connected() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(1, publisher);
        let controller = sender.row_lifecycle_controller();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            done_tx.send(owner.bind(bridge()).run_with_outcome(|_| {})).unwrap();
        });
        drop(sender);
        done_rx.recv_timeout(Duration::from_secs(2)).expect("controller retained legacy channel").unwrap();
        thread.join().unwrap();
        assert!(controller.sender.upgrade().is_none());
    }

    #[test]
    fn row_fifo_input_drop_wakes_actually_parked_owner() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(1, publisher);
        let controller = sender.row_lifecycle_controller();
        let owner_controller = controller.clone();
        let (rows, _ui) = row_preparation_handoff().unwrap();
        let (notice_tx, notice_rx) = mpsc::sync_channel(1);
        *controller.state.before_recv.lock().unwrap() = Some(notice_tx);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            done_tx.send(owner.bind(bridge()).run_with_row_activity(rows, &owner_controller,
                service, |_| {}, |_| {})).unwrap();
        });
        notice_rx.recv_timeout(Duration::from_secs(2)).expect("owner not parked");
        drop(sender);
        let exit = done_rx.recv_timeout(Duration::from_secs(2)).expect("input drop did not wake").unwrap();
        thread.join().unwrap();
        assert_eq!(exit.reason, RowOwnerExitReason::InputClosed);
    }

    #[test]
    fn row_fifo_ui_close_wakes_parked_owner_and_closes_queued_admission() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(1, publisher);
        let controller = sender.row_lifecycle_controller();
        let owner_controller = controller.clone();
        let (rows, mut ui) = row_preparation_handoff().unwrap();
        let (notice_tx, notice_rx) = mpsc::sync_channel(1);
        *controller.state.before_recv.lock().unwrap() = Some(notice_tx);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            done_tx.send(owner.bind(bridge()).run_with_row_activity(rows, &owner_controller,
                service, |_| {}, |_| {})).unwrap();
        });
        notice_rx.recv_timeout(Duration::from_secs(2)).expect("owner not parked");
        ui.close();
        let exit = done_rx.recv_timeout(Duration::from_secs(2)).expect("UI close did not wake").unwrap();
        thread.join().unwrap();
        assert_eq!(exit.reason, RowOwnerExitReason::RowUiClosed);
        assert!(matches!(sender.try_send(OwnerEvent::Command(rune_editor::Command::Undo)),
            Err(TrySendError::Disconnected(_))));
    }

    #[test]
    fn row_fifo_full_wake_after_clear_and_concurrent_stop_preserve_data() {
        for stop_during_pause in [false, true] {
            let (publisher, _updates) = host_updates();
            let (sender, _replies, owner) = pending_editor_transport(1, publisher);
            let controller = sender.row_lifecycle_controller();
            let owner_controller = controller.clone();
            sender.try_send(OwnerEvent::Command(rune_editor::Command::Insert("x".into()))).unwrap();
            let (rows, _ui) = row_preparation_handoff().unwrap();
            let (entered_tx, entered_rx) = mpsc::sync_channel(1);
            let resume = Arc::new(std::sync::Barrier::new(2));
            *controller.state.service_pause.lock().unwrap() = Some((entered_tx, resume.clone()));
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let thread = std::thread::spawn(move || {
                let mut edits = 0;
                let mut observations = Vec::new();
                let result = owner.bind(bridge()).run_with_row_activity(rows, &owner_controller,
                    snapshot_error, |step| { assert!(step.edit.unwrap()); edits += 1; },
                    |row| {
                        let error = row.unwrap_err();
                        if !observations.is_empty() { owner_controller.request_stop(); }
                        observations.push(error);
                    });
                done_tx.send((result, edits, observations)).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).expect("clear/service barrier not reached");
            schedule_row_wake(&controller.sender.upgrade().unwrap(), &controller.state);
            assert!(controller.state.pending.load(Ordering::Acquire)); // Full, no control slot.
            if stop_during_pause { controller.request_stop(); }
            resume.wait();
            let (result, edits, observations) = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let exit = result.unwrap();
            thread.join().unwrap();
            if stop_during_pause {
                assert_eq!(edits, 0);
                assert_eq!(observations, ["snapshot:"]);
                assert!(matches!(exit.undelivered.as_slice(),
                    [OwnerEvent::Command(rune_editor::Command::Insert(text))] if text == "x"));
            } else {
                assert_eq!(edits, 1);
                assert_eq!(observations, ["snapshot:", "snapshot:x"]);
                assert!(exit.undelivered.is_empty());
            }
        }
    }

    #[test]
    fn row_fifo_initial_publication_failure_closes_rows_and_returns_input() {
        let (publisher, updates) = host_updates();
        drop(updates);
        let (sender, _replies, owner) = pending_editor_transport(1, publisher);
        let controller = sender.row_lifecycle_controller();
        sender.try_send(OwnerEvent::Evaluate { request_id: 9, source: "not run".into() }).unwrap();
        let (rows, ui) = row_preparation_handoff().unwrap();
        let error = owner.bind(bridge()).run_with_row_activity(rows, &controller, service,
            |_| panic!("event ran during failed init"), |_| panic!("rows serviced during failed init"))
            .unwrap_err();
        assert!(matches!(error.undelivered.as_slice(),
            [OwnerEvent::Evaluate { request_id: 9, source }] if source == "not run"));
        assert!(matches!(ui.next_request(), Err(rune_display::retained_row_handoff::RowHandoffError::Disconnected)));
    }

    #[test]
    fn row_fifo_idle_reply_wakes_same_owner_and_stop_revokes_receipt() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(2, publisher);
        let controller = sender.row_lifecycle_controller();
        let owner_controller = controller.clone();
        let (rows, mut ui) = row_preparation_handoff().unwrap();
        let (registered_tx, registered_rx) = mpsc::sync_channel(1);
        *controller.state.before_recv.lock().unwrap() = Some(registered_tx);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            // Host is constructed on the owner thread, not moved from a UI runtime.
            let bridge = bridge();
            let (scope, text) = scope_and_text(&bridge);
            let mut rows = rows;
            rows.prepare(scope, &text).unwrap();
            let result = owner.bind(bridge).run_with_row_activity(rows, &owner_controller, service, |_| {}, |_| {});
            done_tx.send(result).unwrap();
        });
        registered_rx.recv_timeout(Duration::from_secs(2)).expect("owner never registered idle");
        let request = ui.next_live_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: request.key(), ui_token: 11 }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let token = loop {
            if let Some(token) = ui.next_live_acceptance().unwrap() { break token; }
            assert!(ui.next_live_request().unwrap().is_none());
            wait_ui(&mut ui, deadline);
        };
        assert!(token.is_live());
        controller.request_stop();
        let exit = done_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(exit.reason, RowOwnerExitReason::StopRequested);
        assert!(exit.undelivered.is_empty());
        thread.join().unwrap();
        assert!(!token.is_live());
    }

    #[test]
    fn row_fifo_full_wake_fallback_services_after_edit_without_replay() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(1, publisher);
        let controller = sender.row_lifecycle_controller();
        sender.try_send(OwnerEvent::Command(rune_editor::Command::Insert("x".into()))).unwrap();
        controller.state.enabled.store(true, Ordering::Release);
        schedule_row_wake(&controller.sender.upgrade().unwrap(), &controller.state); // Full: retains pending.
        assert!(controller.state.pending.load(Ordering::Acquire));
        let (rows, _ui) = row_preparation_handoff().unwrap();
        let mut edits = 0;
        let mut observations = Vec::new();
        let exit = owner.bind(bridge()).run_with_row_activity(rows, &controller, snapshot_error,
            |step| { assert!(step.edit.unwrap()); edits += 1; },
            |row| {
                let error = row.unwrap_err();
                if !observations.is_empty() { controller.request_stop(); }
                observations.push(error);
            }).unwrap();
        assert_eq!(edits, 1);
        assert_eq!(observations, ["snapshot:", "snapshot:x"]);
        assert_eq!(exit.reason, RowOwnerExitReason::StopRequested);
        assert!(exit.undelivered.is_empty());
    }

    #[test]
    fn row_fifo_stop_returns_original_unprocessed_events_and_closes_admission() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(2, publisher);
        let controller = sender.row_lifecycle_controller();
        sender.try_send(OwnerEvent::Evaluate { request_id: 77, source: "never run".into() }).unwrap();
        controller.request_stop(); // Before enable: stop is retained, no wake needed.
        let (rows, _ui) = row_preparation_handoff().unwrap();
        let exit = owner.bind(bridge()).run_with_row_activity(rows, &controller, service,
            |_| panic!("input executed after pre-start stop"),
            |_| panic!("row callback ran after pre-start stop")).unwrap();
        assert!(matches!(exit.undelivered.as_slice(),
            [OwnerEvent::Evaluate { request_id: 77, source }] if source == "never run"));
        assert!(matches!(sender.try_send(OwnerEvent::Command(rune_editor::Command::Undo)),
            Err(TrySendError::Disconnected(_))));
    }

    #[test]
    fn row_fifo_rejects_wrong_controller_and_rendezvous_capacity() {
        let (publisher, _updates) = host_updates();
        let (_sender, _replies, owner) = pending_editor_transport(1, publisher);
        let (other_publisher, _other_updates) = host_updates();
        let (other_sender, _other_replies, other_owner) = pending_editor_transport(1, other_publisher);
        let controller = other_sender.row_lifecycle_controller();
        let (rows, _ui) = row_preparation_handoff().unwrap();
        assert!(owner.bind(bridge()).run_with_row_activity(rows, &controller, service, |_| {}, |_| {}).is_err());
        other_owner.reject_unbound();
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(0, publisher);
        let controller = sender.row_lifecycle_controller();
        let (rows, _ui) = row_preparation_handoff().unwrap();
        assert!(owner.bind(bridge()).run_with_row_activity(rows, &controller, service, |_| {}, |_| {}).is_err());
    }

    #[test]
    fn row_fifo_input_producer_drop_stops_despite_controller_sender_clone() {
        let (publisher, _updates) = host_updates();
        let (sender, _replies, owner) = pending_editor_transport(1, publisher);
        let controller = sender.row_lifecycle_controller();
        drop(sender);
        let (rows, _ui) = row_preparation_handoff().unwrap();
        let exit = owner.bind(bridge()).run_with_row_activity(rows, &controller, service, |_| {}, |_| {}).unwrap();
        assert_eq!(exit.reason, RowOwnerExitReason::InputClosed);
    }
}
