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
    cell::RefCell,
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

pub struct FrameView {
    frame: Arc<FrameDisplay>,
    font: NativeFont,
    focus: FocusHandle,
    input: Rc<RefCell<NativeInput>>,
    resize: ResizeTracker,
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
            input: Rc::new(RefCell::new(NativeInput {
                backlog: InputBacklog::new(input, 256),
                policy: InputPolicy::default(),
                rejected,
                quit,
            })),
            resize: ResizeTracker::default(),
        }
    }
    /// Host calls this after the editor signals downstream channel capacity.
    pub fn retry_input(&mut self) -> Result<(), std::sync::mpsc::TrySendError<()>> {
        let result = self.input.borrow_mut().backlog.flush();
        if matches!(result, Err(std::sync::mpsc::TrySendError::Disconnected(()))) {
            self.shutdown_input();
        }
        result
    }
    pub fn take_pending_input(&mut self) -> Vec<InputEvent> {
        self.input.borrow_mut().backlog.drain_pending().collect()
    }
    pub fn input_readiness(&self) -> InputReadiness {
        self.input.borrow().policy.state()
    }
    /// Replay rejected events in original order while physical input is paused.
    /// Ownership returns to the caller when capacity is still insufficient.
    pub fn replay_input(&mut self, event: InputEvent) -> Result<(), InputEvent> {
        let mut input = self.input.borrow_mut();
        if input.policy.state() == InputReadiness::Disconnected {
            return Err(event);
        }
        input.backlog.submit(event)
    }
    /// Host calls only after all rejected events have been replayed/reported.
    /// Accepted backlog must drain first, preventing later physical input from
    /// overtaking recovery events. Disconnection can never be resumed.
    pub fn resume_input(&mut self) -> bool {
        let mut input = self.input.borrow_mut();
        let pending = input.backlog.pending_len();
        input.policy.resume(pending)
    }
    /// Call before releasing the view/window to return accepted undelivered
    /// events through the host rejection callback. A dropped view cannot replay.
    pub fn shutdown_input(&mut self) {
        let (callback, pending) = {
            let mut input = self.input.borrow_mut();
            input.policy.disconnect();
            let callback = input.rejected.clone();
            let pending: Vec<_> = input.backlog.drain_pending().collect();
            (callback, pending)
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
                        if let Some(frame) = update.frame {
                            if let Err(error) = view.present(frame, cx) {
                                eprintln!("Rune frame rejected: {error}");
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = view.update(cx, |view, _| view.shutdown_input());
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
            if NativeInput::submit(&self.input, event.clone()) {
                self.resize.acknowledge(&event);
            }
        }
        let frame = self.frame.clone();
        let pointer_frame = frame.clone();
        let pointer_focus = self.focus.clone();
        let pointer_input = self.input.clone();
        let wheel_input = self.input.clone();
        let metrics = self.font.metrics();
        let font = self.font.clone();
        let input = self.input.clone();
        div()
            .id("rune-frame")
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                let key = &event.keystroke;
                NativeInput::submit(
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
                let position =
                    rune_display::Point { x: event.position.x.into(), y: event.position.y.into() };
                if let Some(hit) = pointer_frame.hit_test(position) {
                    let button = match event.button {
                        MouseButton::Left => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Right => 3,
                        _ => return,
                    };
                    NativeInput::submit(&pointer_input, InputEvent::PointerHit { hit, button });
                    window.focus(&pointer_focus);
                    cx.stop_propagation();
                }
            })
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(metrics.line_height()));
                NativeInput::submit(
                    &wheel_input,
                    InputEvent::Scroll { dx: delta.x.into(), dy: delta.y.into() },
                );
                cx.stop_propagation();
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        let mut target = GpuiTarget {
                            window,
                            cx,
                            origin: bounds.origin,
                            clips: Vec::new(),
                            font: font.clone(),
                            paint_failed: false,
                        };
                        rune_display::paint_frame(&frame, metrics, &mut target);
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
