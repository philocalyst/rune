#![forbid(unsafe_code)]
//! Native GPUI 0.2.2 canvas adapter. No Lisp objects, locks or layout in paint.
//! The host owns scheduling: on a newer mailbox generation, update FrameView
//! and notify once. Idle views do not request animation frames.
use gpui::{
    App, Bounds, ContentMask, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, Point, Render, ScrollWheelEvent, TextRun, Window, canvas, div, fill,
    point, prelude::*, px, rgba, size,
};
use rune_display::{Color, Face, FontMetrics, FrameDisplay, PaintTarget, Rect};
use rune_display::{InputBacklog, InputEvent, ResizeTracker};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod font_geometry;
mod input_policy;
use input_policy::InputPolicy;
pub use input_policy::{InputReadiness, RejectionReason};

#[derive(Debug)]
pub struct InputRejection {
    pub event: InputEvent,
    pub reason: RejectionReason,
}
/// Receives ownership on saturation, pause or host closure. The host must retain
/// or explicitly report rejected input; subsequent physical inputs stay paused.
pub type RejectedInput = Rc<dyn Fn(InputRejection)>;

struct NativeInput {
    backlog: InputBacklog,
    policy: InputPolicy,
    rejected: RejectedInput,
    quit: Arc<AtomicBool>,
}
impl NativeInput {
    fn submit(input: &Rc<RefCell<Self>>, event: InputEvent) -> bool {
        let rejection = {
            let mut input = input.borrow_mut();
            if matches!(&event, InputEvent::Key { key, control: true, .. } if key == "g") {
                input.quit.store(true, Ordering::Release);
            }
            if let Some(reason) = input.policy.rejection() {
                Some((input.rejected.clone(), InputRejection { event, reason }))
            } else {
                match input.backlog.submit(event) {
                    Ok(()) => None,
                    Err(event) => {
                        input.policy.saturate();
                        Some((
                            input.rejected.clone(),
                            InputRejection { event, reason: RejectionReason::Saturated },
                        ))
                    }
                }
            }
        };
        // Callback may schedule UI work; never invoke it with a RefCell borrow.
        if let Some((callback, event)) = rejection {
            callback(event);
            false
        } else {
            true
        }
    }
}

/// Native family/size and measured fixed-cell metrics travel together, so the
/// layout worker and painter cannot accidentally use unrelated font sizes.
#[derive(Clone)]
pub struct NativeFont {
    font: gpui::Font,
    size: Pixels,
    metrics: FontMetrics,
}
impl NativeFont {
    /// Measures on the UI thread before creating a layout worker. GPUI resolves
    /// fallback fonts; like GPUI itself this can panic if no fonts can resolve.
    pub fn measure(
        font: gpui::Font,
        size: f32,
        line_gap: f32,
        cx: &App,
    ) -> Result<Self, &'static str> {
        if !size.is_finite() || size <= 0.0 || !line_gap.is_finite() || line_gap < 0.0 {
            return Err("invalid native font size or line gap");
        }
        let system = cx.text_system();
        let id = system.resolve_font(&font);
        let mut advances = [0.0; 3];
        for (advance, character) in advances.iter_mut().zip(['0', 'i', 'm']) {
            *advance = system
                .advance(id, px(size), character)
                .map_err(|_| "native font advance measurement failed")?
                .width
                .into();
        }
        let ascent: f32 = system.ascent(id, px(size)).into();
        let descent: f32 = system.descent(id, px(size)).into();
        font_geometry::validate(size, advances, ascent, descent, line_gap)?;
        Ok(Self {
            font,
            size: px(size),
            metrics: FontMetrics { advance: advances[0], ascent, descent, line_gap },
        })
    }
    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }
}

#[derive(Clone)]
enum FrameInput {
    Legacy(Rc<RefCell<NativeInput>>),
    #[cfg(feature = "editor")]
    Owner(Rc<RefCell<owner_input::OwnerInput>>),
}
impl FrameInput {
    fn submit(input: &Self, event: InputEvent) -> bool {
        match input {
            Self::Legacy(input) => NativeInput::submit(input, event),
            #[cfg(feature = "editor")]
            Self::Owner(input) => {
                owner_input::OwnerInput::submit(input, editor_transport::OwnerEvent::Input(event))
            }
        }
    }
}
pub struct FrameView {
    frame: Arc<FrameDisplay>,
    font: NativeFont,
    focus: FocusHandle,
    input: FrameInput,
    resize: ResizeTracker,
    #[cfg(feature = "editor")]
    ime: Option<platform_handler::ImeBinding>,
}
impl FrameView {
    pub fn new(
        frame: Arc<FrameDisplay>,
        font: NativeFont,
        input: rune_display::InputSender,
        quit: Arc<AtomicBool>,
        rejected: RejectedInput,
        cx: &mut Context<Self>,
    ) -> Self {
        // GPUI's entity-release callback still owns &mut FrameView, so accepted
        // queued input can be returned before the backlog itself disappears.
        // No strong entity is captured and no subscription cycle is introduced.
        cx.on_release(|view, _| view.shutdown_input()).detach();
        Self {
            frame,
            font,
            focus: cx.focus_handle(),
            input: FrameInput::Legacy(Rc::new(RefCell::new(NativeInput {
                backlog: InputBacklog::new(input, 256),
                policy: InputPolicy::default(),
                rejected,
                quit,
            }))),
            resize: ResizeTracker::default(),
            #[cfg(feature = "editor")]
            ime: None,
        }
    }
    /// Editor-host constructor. Ordinary input and captured composition requests
    /// share one bounded ordered FIFO; a separate explicit reply slot reports edits.
    /// This does not register an IME InputHandler or invent platform text context.
    #[cfg(feature = "editor")]
    pub fn new_editor(
        frame: Arc<FrameDisplay>,
        font: NativeFont,
        sender: editor_transport::NativeEditorSender,
        replies: editor_transport::NativeEditorReplies,
        quit: Arc<AtomicBool>,
        rejected: owner_input::RejectedOwnerInput,
        outcome: owner_input::CompositionOutcome,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|view, _| view.shutdown_input()).detach();
        Self {
            frame,
            font,
            focus: cx.focus_handle(),
            input: FrameInput::Owner(owner_input::OwnerInput::new(
                sender, replies, quit, rejected, outcome,
            )),
            resize: ResizeTracker::default(),
            #[cfg(feature = "editor")]
            ime: None,
        }
    }
    /// Connect UI C-g to the same request-scoped channel configured on its
    /// owner worker. Legacy quit observability remains independent.
    #[cfg(feature = "editor")]
    pub fn set_evaluation_quit(
        &mut self,
        signal: evaluation_quit::EvaluationQuit,
    ) -> Result<(), &'static str> {
        match &self.input {
            FrameInput::Owner(input) => {
                input.borrow_mut().set_evaluation_quit(signal);
                Ok(())
            }
            FrameInput::Legacy(_) => Err("evaluator quit requires an editor owner view"),
        }
    }
    /// Submit one UI-owned evaluator request through the same ordered FIFO.
    /// True means admitted or retained in the UI backlog; never resend it.
    /// On false the caller keeps its draft; mandatory rejection reporting also
    /// receives the original event. Legacy fixture views return false.
    #[cfg(feature = "editor")]
    pub fn submit_evaluation(&self, request_id: u64, source: String) -> bool {
        match &self.input {
            FrameInput::Owner(input) => owner_input::OwnerInput::submit(
                input,
                editor_transport::OwnerEvent::Evaluate { request_id, source },
            ),
            FrameInput::Legacy(_) => false,
        }
    }
    /// Consume explicit composition outcomes before retrying the owner FIFO.
    /// This order frees its reserved reply slot without letting input overtake.
    pub fn retry_input(&mut self) -> Result<(), std::sync::mpsc::TrySendError<()>> {
        let result = match &self.input {
            FrameInput::Legacy(input) => input.borrow_mut().backlog.flush(),
            #[cfg(feature = "editor")]
            FrameInput::Owner(input) => owner_input::OwnerInput::on_wake(input),
        };
        if matches!(result, Err(std::sync::mpsc::TrySendError::Disconnected(()))) {
            self.shutdown_input();
        }
        result
    }
    /// Legacy FIFO only. Editor hosts must recover the complete OwnerEvent FIFO
    /// with take_pending_owner_input, not extract keys around a composition.
    pub fn take_pending_input(&mut self) -> Vec<InputEvent> {
        match &self.input {
            FrameInput::Legacy(input) => input.borrow_mut().backlog.drain_pending().collect(),
            #[cfg(feature = "editor")]
            FrameInput::Owner(_) => Vec::new(),
        }
    }
    pub fn input_readiness(&self) -> InputReadiness {
        match &self.input {
            FrameInput::Legacy(input) => input.borrow().policy.state(),
            #[cfg(feature = "editor")]
            FrameInput::Owner(input) => input.borrow().readiness(),
        }
    }
    pub fn replay_input(&mut self, event: InputEvent) -> Result<(), InputEvent> {
        match &self.input {
            FrameInput::Legacy(input) => {
                let mut input = input.borrow_mut();
                if input.policy.state() == InputReadiness::Disconnected {
                    return Err(event);
                }
                input.backlog.submit(event)
            }
            #[cfg(feature = "editor")]
            FrameInput::Owner(input) => input
                .borrow_mut()
                .replay(editor_transport::OwnerEvent::Input(event))
                .map_err(|event| match event {
                    editor_transport::OwnerEvent::Input(event) => event,
                    _ => unreachable!("replay returns its same input ownership"),
                }),
        }
    }
    pub fn resume_input(&mut self) -> bool {
        match &self.input {
            FrameInput::Legacy(input) => {
                let mut input = input.borrow_mut();
                let n = input.backlog.pending_len();
                input.policy.resume(n)
            }
            #[cfg(feature = "editor")]
            FrameInput::Owner(input) => input.borrow_mut().resume(),
        }
    }
    /// Returns Err unchanged on a legacy view. In editor mode rejected ownership
    /// goes to its mandatory callback; never re-emit an accepted composition.
    #[cfg(feature = "editor")]
    pub fn submit_composition(
        &mut self,
        request: editor_transport::CompositionRequest,
    ) -> Result<bool, editor_transport::CompositionRequest> {
        match &self.input {
            FrameInput::Legacy(_) => Err(request),
            FrameInput::Owner(input) => Ok(owner_input::OwnerInput::submit(
                input,
                editor_transport::OwnerEvent::Composition(request),
            )),
        }
    }
    #[cfg(feature = "editor")]
    pub fn take_pending_owner_input(&mut self) -> Option<Vec<editor_transport::OwnerEvent>> {
        match &self.input {
            FrameInput::Legacy(_) => None,
            FrameInput::Owner(input) => Some(input.borrow_mut().take_pending()),
        }
    }
    #[cfg(feature = "editor")]
    pub fn replay_owner_input(
        &mut self,
        event: editor_transport::OwnerEvent,
    ) -> Result<(), editor_transport::OwnerEvent> {
        match &self.input {
            FrameInput::Legacy(_) => Err(event),
            FrameInput::Owner(input) => input.borrow_mut().replay(event),
        }
    }
    pub fn shutdown_input(&mut self) {
        #[cfg(feature = "editor")]
        if let Some(ime) = &self.ime {
            ime.state.borrow_mut().active(false);
        }
        let input = match &self.input {
            FrameInput::Legacy(input) => input,
            #[cfg(feature = "editor")]
            FrameInput::Owner(input) => return owner_input::OwnerInput::shutdown(input),
        };
        let (callback, pending) = {
            let mut input = input.borrow_mut();
            input.policy.disconnect();
            (input.rejected.clone(), input.backlog.drain_pending().collect::<Vec<_>>())
        };
        for event in pending {
            callback(InputRejection { event, reason: RejectionReason::HostDisconnected });
        }
    }

    /// Called on the UI thread after a worker publication wake. No idle polling
    /// or animation loop: only a strictly newer compatible frame notifies GPUI.
    pub fn present(
        &mut self,
        frame: Arc<FrameDisplay>,
        cx: &mut Context<Self>,
    ) -> Result<bool, &'static str> {
        if frame.protocol_version != rune_display::DISPLAY_PROTOCOL_VERSION {
            return Err("incompatible display protocol");
        }
        if frame.generation <= self.frame.generation {
            return Ok(false);
        }
        self.frame = frame;
        cx.notify();
        Ok(true)
    }

    /// UI-thread entry point for hosts using the latest-only display mailbox.
    /// The host must arrange one foreground wake after publishing. This method
    /// does not start a timer or hold the mailbox lock during paint.
    pub fn present_latest(
        &mut self,
        mailbox: &rune_display::DisplayMailbox,
        cx: &mut Context<Self>,
    ) -> Result<bool, &'static str> {
        match mailbox.latest() {
            Some(frame) => self.present(frame, cx),
            None => Ok(false),
        }
    }

    /// Foreground async bridge. Retain/detach the returned GPUI task; it awaits
    /// executor-neutral wakes and never blocks an executor thread on recv().
    pub fn watch_updates(
        mut updates: rune_display::HostUpdates,
        cx: &mut Context<Self>,
    ) -> gpui::Task<()> {
        cx.spawn(async move |view, cx| {
            while let Some(update) = updates.next().await {
                if view
                    .update(cx, |view, cx| {
                        if update.input_capacity {
                            let _ = view.retry_input();
                        }
                        #[cfg(feature = "editor")]
                        let platform_handled = view.sync_platform(cx);
                        #[cfg(not(feature = "editor"))]
                        let platform_handled = false;
                        if !platform_handled {
                            if let Some(frame) = update.frame {
                                if let Err(error) = view.present(frame, cx) {
                                    eprintln!("Rune frame rejected: {error}");
                                }
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = view.update(cx, |view, cx| {
                view.shutdown_input();
                cx.notify();
            });
        })
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for FrameView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        if let Some(event) = self.resize.pending(
            viewport.width.into(),
            viewport.height.into(),
            window.scale_factor(),
        ) {
            if FrameInput::submit(&self.input, event.clone()) {
                self.resize.acknowledge(&event);
            }
        }
        let frame = self.frame.clone();
        let pointer_frame = frame.clone();
        // Pointer events are window-relative; prepared frame coordinates are
        // canvas-local, just like the paint target and platform text geometry.
        let painted_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let pointer_bounds = painted_bounds.clone();
        let pointer_focus = self.focus.clone();
        let pointer_input = self.input.clone();
        let wheel_input = self.input.clone();
        let metrics = self.font.metrics();
        let font = self.font.clone();
        let input = self.input.clone();
        #[cfg(feature = "editor")]
        let ime_for_key = self.ime.clone();
        #[cfg(feature = "editor")]
        let ime_for_pointer = self.ime.clone();
        #[cfg(feature = "editor")]
        let ime_for_paint = self.ime.clone();
        #[cfg(feature = "editor")]
        let input_entity = _cx.entity();
        #[cfg(feature = "editor")]
        let paint_focus = self.focus.clone();
        div()
            .id("rune-frame")
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                #[cfg(feature = "editor")]
                if let Some(ime) = &ime_for_key {
                    platform_handler::keydown(ime, &input, event, window, cx);
                    return;
                }
                let _ = window;
                let key = &event.keystroke;
                FrameInput::submit(
                    &input,
                    rune_display::InputEvent::Key {
                        key: key.key.clone(),
                        text: key.key_char.clone(),
                        control: key.modifiers.control,
                        meta: key.modifiers.alt,
                        shift: key.modifiers.shift,
                    },
                );
                cx.stop_propagation();
            })
            .on_any_mouse_down(move |event: &MouseDownEvent, window, cx| {
                let Some(bounds) = pointer_bounds.get() else {
                    return;
                };
                let position = rune_display::Point {
                    x: (event.position.x - bounds.origin.x).into(),
                    y: (event.position.y - bounds.origin.y).into(),
                };
                if let Some(hit) = pointer_frame.hit_test(position) {
                    let button = match event.button {
                        MouseButton::Left => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Right => 3,
                        _ => return,
                    };
                    #[cfg(feature = "editor")]
                    if let Some(ime) = &ime_for_pointer {
                        if button == 1 {
                            ime.state.borrow_mut().invalidate(true);
                        } else {
                            ime.state.borrow_mut().cancel_preedit();
                        }
                    }
                    FrameInput::submit(&pointer_input, InputEvent::PointerHit { hit, button });
                    window.focus(&pointer_focus);
                    cx.stop_propagation();
                }
            })
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(metrics.line_height()));
                FrameInput::submit(
                    &wheel_input,
                    InputEvent::Scroll { dx: delta.x.into(), dy: delta.y.into() },
                );
                cx.stop_propagation();
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        painted_bounds.set(Some(bounds));
                        #[cfg(feature = "editor")]
                        let overlay = ime_for_paint.as_ref().and_then(|ime| {
                            ime.bounds.set(Some(bounds));
                            // A transient unavailable selection is reported as None
                            // by callbacks, not by removing the handler and silently
                            // dropping ordinary platform text between owner frames.
                            window.handle_input(
                                &paint_focus,
                                gpui::ElementInputHandler::new(bounds, input_entity.clone()),
                                cx,
                            );
                            ime.state.borrow().overlay()
                        });
                        #[cfg(feature = "editor")]
                        let painted_frame = if let Some(overlay) = &overlay {
                            let mut base = (*frame).clone();
                            for w in &mut base.windows {
                                if overlay.windows.iter().any(|p| {
                                    p.window_id == w.window_id
                                        && p.buffer_id == w.buffer_id
                                        && p.revision == w.revision
                                }) {
                                    w.cursor = None;
                                }
                            }
                            Arc::new(base)
                        } else {
                            frame.clone()
                        };
                        #[cfg(not(feature = "editor"))]
                        let painted_frame = frame.clone();
                        let mut target = GpuiTarget {
                            window,
                            cx,
                            origin: bounds.origin,
                            clips: Vec::new(),
                            font: font.clone(),
                            paint_failed: false,
                        };
                        rune_display::paint_frame(&painted_frame, metrics, &mut target);
                        #[cfg(feature = "editor")]
                        if let Some(overlay) = overlay {
                            rune_display::paint_frame(&overlay, metrics, &mut target);
                            target.push_clip(overlay.bounds);
                            for window in &overlay.windows {
                                for row in &window.rows {
                                    for cluster in &row.clusters {
                                        target.fill(
                                            Rect {
                                                x: cluster.bounds.x,
                                                y: row.baseline + 1.,
                                                width: cluster.bounds.width,
                                                height: 1.,
                                            },
                                            Color(240, 210, 120, 255),
                                        );
                                    }
                                }
                            }
                            target.pop_clip();
                        }
                    },
                )
                .size_full(),
            )
    }
}
struct GpuiTarget<'a> {
    window: &'a mut Window,
    cx: &'a mut App,
    origin: Point<Pixels>,
    clips: Vec<Rect>,
    font: NativeFont,
    paint_failed: bool,
}
fn native_color(c: Color) -> gpui::Hsla {
    rgba(u32::from_be_bytes([c.0, c.1, c.2, c.3])).into()
}
impl GpuiTarget<'_> {
    fn bounds(&self, r: Rect) -> Bounds<Pixels> {
        Bounds {
            origin: point(self.origin.x + px(r.x), self.origin.y + px(r.y)),
            size: size(px(r.width), px(r.height)),
        }
    }
    fn mask(&self) -> Option<ContentMask<Pixels>> {
        self.clips.last().copied().map(|r| ContentMask { bounds: self.bounds(r) })
    }
}
impl PaintTarget for GpuiTarget<'_> {
    fn push_clip(&mut self, r: Rect) {
        let intersection = self.clips.last().copied().map_or(Some(r), |old| old.intersection(r));
        self.clips
            .push(intersection.unwrap_or(Rect { x: r.x, y: r.y, width: 0.0, height: 0.0 }));
    }
    fn pop_clip(&mut self) {
        self.clips.pop();
    }
    fn fill(&mut self, r: Rect, color: Color) {
        let bounds = self.bounds(r);
        let mask = self.mask();
        self.window
            .with_content_mask(mask, |window| window.paint_quad(fill(bounds, native_color(color))));
    }
    fn text(&mut self, text: &str, origin: rune_display::Point, face: Face, metrics: FontMetrics) {
        let run = TextRun {
            len: text.len(),
            font: self.font.font.clone(),
            color: native_color(face.foreground),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = self.window.text_system().shape_line(
            text.to_owned().into(),
            self.font.size,
            &[run],
            Some(px(metrics.advance)),
        );
        // GPUI centers the shaped ascent/descent within line_height. Convert
        // our explicit baseline using the actual shaped metrics, not fixture
        // ascent, so fallback fonts do not silently move glyph baselines.
        let line_height = px(metrics.line_height());
        let baseline_offset = (line_height - shaped.ascent - shaped.descent) / 2.0 + shaped.ascent;
        let point =
            point(self.origin.x + px(origin.x), self.origin.y + px(origin.y) - baseline_offset);
        let mask = self.mask();
        let cx = &mut *self.cx;
        let result = self
            .window
            .with_content_mask(mask, |window| shaped.paint(point, line_height, window, cx));
        if let Err(error) = result {
            // One report per frame, not a log storm per visible scalar.
            if !self.paint_failed {
                eprintln!("Rune glyph paint failed: {error}");
            }
            self.paint_failed = true;
        }
    }
}

#[cfg(feature = "editor")]
pub mod composition;
#[cfg(feature = "editor")]
pub mod editor_bridge;
#[cfg(feature = "editor")]
pub mod editor_transport;
#[cfg(feature = "editor")]
pub mod evaluation_quit;
#[cfg(feature = "editor")]
pub mod ime_context;

#[cfg(feature = "editor")]
pub mod owner_input;

#[cfg(feature = "editor")]
pub mod platform_context;

#[cfg(feature = "editor")]
mod ime_keys;
#[cfg(feature = "editor")]
pub mod platform_handler;
#[cfg(feature = "editor")]
pub mod platform_state;
