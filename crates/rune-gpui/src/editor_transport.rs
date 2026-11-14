//! Ordered bounded owner handoff, explicit composition outcome and startup gate.
//! No platform InputHandler or FrameView envelope wiring is implied.
use crate::{
    composition::{CommittedText, CompositionToken},
    editor_bridge::{BridgeError, EditorBridge},
};
use rune_display::{FrameDisplay, HostPublisher, InputEvent};
use rune_editor::{DisplayDocument, EditorHost};
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
};
#[derive(Debug)]
pub struct CompositionRequest {
    pub token: CompositionToken,
    pub committed: CommittedText,
}
#[derive(Debug)]
pub enum OwnerEvent {
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
    sender: SyncSender<OwnerEvent>,
    reply: Arc<Mutex<ReplyState>>,
}
pub struct NativeEditorReplies(Arc<Mutex<ReplyState>>);
#[must_use = "Bind inside the owner callback or explicitly reject_unbound"]
pub struct NativeEditorOwner {
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
    receiver: Receiver<OwnerEvent>,
    reply: Arc<Mutex<ReplyState>>,
    publisher: HostPublisher,
    initialized: bool,
    visited_saver: Option<VisitedSaver<H>>,
    key_dispatcher: Option<(NativeKeyDispatcher<H>, crate::evaluation_quit::EvaluationQuit)>,
    evaluator: Option<fn(&mut H, &str) -> Result<(), String>>,
    interruptible: Option<(InterruptibleEvaluator<H>, crate::evaluation_quit::EvaluationQuit)>,
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
    receiver.try_iter().collect()
}
pub fn pending_editor_transport(
    capacity: usize,
    publisher: HostPublisher,
) -> (NativeEditorSender, NativeEditorReplies, NativeEditorOwner) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let reply = Arc::new(Mutex::new(ReplyState { outstanding: false, ack: None, ui_alive: true }));
    (
        NativeEditorSender { sender, reply: reply.clone() },
        NativeEditorReplies(reply.clone()),
        NativeEditorOwner { receiver, reply, publisher },
    )
}
impl NativeEditorOwner {
    pub fn reject_unbound(self) -> Vec<OwnerEvent> {
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
            receiver: self.receiver,
            reply: self.reply,
            publisher: self.publisher,
            initialized: false,
            visited_saver: None,
            key_dispatcher: None,
            evaluator: None,
            interruptible: None,
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
        self.publisher.publish(frame).map_err(BridgeError::Publish)
    }
    fn process(&mut self, event: OwnerEvent) -> OwnerStep {
        let mut evaluation = None;
        let mut key_outcome = None;
        let mut force_frame = false;
        let mut save = None;
        let mut preserve_context = false;
        let edit = match event {
            OwnerEvent::SaveVisited { request_id, max_bytes } => {
                match self.bridge.snapshot().map(|snapshot| snapshot.sequence) {
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
                        match self.bridge.snapshot() {
                            Ok(after) => {
                                let changed = before != after.sequence;
                                preserve_context = !changed;
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
                let before = self.bridge.snapshot().map(|s| s.sequence);
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
                        match self.bridge.snapshot() {
                            Ok(after) => Ok(after.sequence != before),
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
                    let before = if request.key.len()>128 {Err(BridgeError::UnsupportedInput)} else {self.bridge.snapshot().map(|s|s.sequence)};
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
                                self.bridge.snapshot().map(|s| s.sequence != before)
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
                    let before = self.bridge.snapshot().map(|s| s.sequence);
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
                                self.bridge.snapshot().map(|s| s.sequence != before)
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
