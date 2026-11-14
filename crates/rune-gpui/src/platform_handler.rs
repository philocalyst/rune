//! Actual GPUI 0.2.2 platform adapter. Registration is paint-scoped and opt-in;
//! compiler/tests do not establish native platform behavior.
use crate::{
    FrameInput, FrameView, InputReadiness, NativeFont,
    editor_transport::{NativeEditorReplies, NativeEditorSender, OwnerEvent},
    ime_keys::{self, ImeKey},
    owner_input::{CompositionOutcome, OwnerInput, RejectedOwnerInput},
    platform_context::PlatformReader,
    platform_state::{PlatformError, PlatformInputState},
};
use gpui::{
    Bounds, Context, EntityInputHandler, KeyDownEvent, Pixels, Point, UTF16Selection, Window,
    point, px, size,
};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Clone, Debug)]
pub enum ImeOperation {
    Replace {
        range: Option<Range<usize>>,
        text: String,
    },
    Preedit {
        range: Option<Range<usize>>,
        text: String,
        selection: Option<Range<usize>>,
    },
    Unmark,
    Shortcut(String),
    Outcome,
}
#[derive(Clone, Debug)]
pub struct NativeImeFailure {
    pub operation: ImeOperation,
    pub message: String,
}
pub type ImeFailure = Rc<dyn Fn(NativeImeFailure)>;
#[derive(Clone)]
pub(crate) struct ImeBinding {
    pub(crate) state: Rc<RefCell<PlatformInputState>>,
    pub(crate) bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    failure: ImeFailure,
    pub(crate) scale: Rc<Cell<f32>>,
}
impl ImeBinding {
    fn report(&self, operation: ImeOperation, error: impl std::fmt::Debug) {
        (self.failure)(NativeImeFailure { operation, message: format!("{error:?}") });
    }
}
impl FrameView {
    /// Owner must use bind_ime_initialized with the paired publisher. Printable
    /// text comes only from platform callbacks; native commands have a fixed
    /// first-light policy rather than guessed asynchronous keymap prefixes.
    pub fn new_ime(
        frame: Arc<rune_display::FrameDisplay>,
        font: NativeFont,
        sender: NativeEditorSender,
        replies: NativeEditorReplies,
        platform: PlatformReader,
        quit: Arc<AtomicBool>,
        rejected: RejectedOwnerInput,
        outcome: CompositionOutcome,
        failure: ImeFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, PlatformError> {
        let mut state =
            PlatformInputState::new(platform, rune_display::FixedMetrics(font.metrics()))?;
        let prepared = state.sync().ok_or(PlatformError::Unavailable)?;
        if state.selected_range().is_none() || prepared.generation < frame.generation {
            return Err(PlatformError::Unavailable);
        }
        let frame = prepared;
        let state = Rc::new(RefCell::new(state));
        let binding = ImeBinding {
            state: state.clone(),
            bounds: Rc::new(Cell::new(None)),
            failure,
            scale: Rc::new(Cell::new(window.scale_factor())),
        };
        let notify = binding.clone();
        let wrapped = Rc::new(move |ack: crate::editor_transport::CompositionAck| {
            notify.state.borrow_mut().acknowledge(&ack);
            if let Err(error) = &ack.outcome {
                notify.report(ImeOperation::Outcome, error);
            }
            outcome(ack);
        });
        let mut view = Self::new_editor(frame, font, sender, replies, quit, rejected, wrapped, cx);
        view.ime = Some(binding);
        cx.on_blur(&view.focus, window, |view, _, cx| {
            if let Some(ime) = &view.ime {
                ime.state.borrow_mut().active(false);
            }
            cx.notify();
        })
        .detach();
        cx.on_focus(&view.focus, window, |view, window, cx| {
            if let Some(ime) = &view.ime {
                ime.state.borrow_mut().active(window.is_window_active());
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_activation(window, |view, window, cx| {
            if let Some(ime) = &view.ime {
                ime.state
                    .borrow_mut()
                    .active(window.is_window_active() && view.focus.is_focused(window));
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_bounds(window, |view, window, cx| {
            if let Some(ime) = &view.ime {
                let viewport = window.viewport_size();
                let scale = window.scale_factor();
                let old = ime.scale.replace(scale);
                let resized: f32 = viewport.width.into();
                let height: f32 = viewport.height.into();
                if resized != view.frame.bounds.width
                    || height != view.frame.bounds.height
                    || scale != old
                {
                    ime.state.borrow_mut().invalidate(true);
                } else {
                    ime.state.borrow_mut().cancel_preedit();
                }
            }
            cx.notify();
        })
        .detach();
        if let Some(ime) = &view.ime {
            ime.state
                .borrow_mut()
                .active(window.is_window_active() && view.focus.is_focused(window));
        }
        Ok(view)
    }
    /// Pair prepared text and paint atomically before a native view update. An
    /// absent/invalidated context disables platform queries rather than guessing.
    pub(crate) fn sync_platform(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(ime) = self.ime.clone() else {
            return false;
        };
        let frame = ime.state.borrow_mut().sync();
        if let Some(frame) = frame {
            if let Err(error) = self.present(frame, cx) {
                ime.report(ImeOperation::Outcome, error);
            }
        }
        cx.notify();
        true
    }
    pub fn cancel_platform_composition(&mut self, cx: &mut Context<Self>) {
        if let Some(ime) = &self.ime {
            ime.state.borrow_mut().cancel_preedit();
        }
        cx.notify();
    }
    fn enqueue_platform(&self, event: OwnerEvent) {
        match &self.input {
            FrameInput::Owner(input) => {
                OwnerInput::submit(input, event);
            }
            FrameInput::Legacy(_) => {
                unreachable!("IME constructor uses the ordered owner transport")
            }
        }
    }
}
pub(crate) fn keydown(
    ime: &ImeBinding,
    input: &FrameInput,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let key = &event.keystroke;
    let composing = ime.state.borrow().composing();
    match ime_keys::route(
        &key.key,
        key.modifiers.control,
        key.modifiers.platform,
        key.modifiers.alt,
        key.modifiers.shift,
        composing,
    ) {
        ImeKey::Platform => {}
        ImeKey::Command(command) => {
            ime.state.borrow_mut().defer_scoped_input();
            if let FrameInput::Owner(input) = input {
                OwnerInput::submit(input, OwnerEvent::Command(command));
            }
            window.prevent_default();
            cx.stop_propagation();
        }
        ImeKey::Cancel => {
            ime.state.borrow_mut().cancel_preedit();
            FrameInput::submit(
                input,
                rune_display::InputEvent::Key {
                    key: key.key.clone(),
                    text: None,
                    control: key.modifiers.control,
                    meta: key.modifiers.alt,
                    shift: key.modifiers.shift,
                },
            );
            window.prevent_default();
            cx.stop_propagation();
        }
        ImeKey::Unsupported => {
            ime.report(
                ImeOperation::Shortcut(format!("{key:?}")),
                "unsupported native IME shortcut/selection policy",
            );
            window.prevent_default();
            cx.stop_propagation();
        }
    }
}
impl EntityInputHandler for FrameView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let ime = self.ime.as_ref()?;
        let result = ime.state.borrow().text_for_range(range).ok()?;
        *adjusted = Some(result.0);
        Some(result.1)
    }
    fn selected_text_range(
        &mut self,
        ignore_disabled: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !ignore_disabled && self.input_readiness() != InputReadiness::Ready {
            return None;
        }
        let range = self.ime.as_ref()?.state.borrow().selected_range()?;
        Some(UTF16Selection { range, reversed: false })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.ime.as_ref()?.state.borrow().marked_range()
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ime) = self.ime.clone() else {
            return;
        };
        let result = ime.state.borrow_mut().unmark();
        match result {
            Ok(Some(event)) => self.enqueue_platform(event),
            Ok(None) => {}
            Err(error) => ime.report(ImeOperation::Unmark, error),
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ime) = self.ime.clone() else {
            return;
        };
        let result = ime.state.borrow_mut().commit(range.clone(), text);
        match result {
            Ok(event) => self.enqueue_platform(event),
            Err(error) => ime.report(ImeOperation::Replace { range, text: text.into() }, error),
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ime) = self.ime.clone() else {
            return;
        };
        let result = ime.state.borrow_mut().replace_and_mark(range.clone(), text, selected.clone());
        if let Err(error) = result {
            ime.report(
                ImeOperation::Preedit { range, text: text.into(), selection: selected },
                error,
            );
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let r = self.ime.as_ref()?.state.borrow().bounds_for_range(range)?;
        Some(Bounds {
            origin: point(bounds.origin.x + px(r.x), bounds.origin.y + px(r.y)),
            size: size(px(r.width), px(r.height)),
        })
    }
    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let ime = self.ime.as_ref()?;
        let b = ime.bounds.get()?;
        let x: f32 = (p.x - b.origin.x).into();
        let y: f32 = (p.y - b.origin.y).into();
        ime.state.borrow().character_index_for_point(rune_display::Point { x, y })
    }
}
