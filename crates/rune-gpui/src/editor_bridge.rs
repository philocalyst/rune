//! Reconstructed owner-host bridge. Source-only until recovery checks run.
//! No mutable Session escape, runtime Context, raw Lisp value or GC root enters paint.
use crate::composition::{CommittedText, CompositionContext};
use rune_display::{
    Color, DISPLAY_PROTOCOL_VERSION, FixedMetrics, FrameDisplay, InputEvent, LayoutError, Rect,
    TextSnapshot, WindowSpec, layout_window,
};
use rune_editor::{Command, Dispatch, DisplayDocument, EditorError, EditorHost, Key};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Debug)]
pub enum BridgeError {
    Editor(EditorError),
    Layout(LayoutError),
    UnsupportedInput,
    StaleHit,
    GenerationExhausted,
    Publish(&'static str),
}
impl From<EditorError> for BridgeError {
    fn from(e: EditorError) -> Self {
        Self::Editor(e)
    }
}
impl From<LayoutError> for BridgeError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextSource {
    KeyDown,
    CommittedText,
}
pub struct EditorBridge<H: EditorHost>
where
    H::Doc: DisplayDocument,
{
    host: H,
    metrics: FixedMetrics,
    bounds: Rect,
    scale: f32,
    generation: u64,
    text_source: TextSource,
    anchors: BTreeMap<u64, (u64, usize)>,
}
impl<H: EditorHost> EditorBridge<H>
where
    H::Doc: DisplayDocument,
{
    pub fn new(mut host: H, metrics: FixedMetrics, bounds: Rect) -> Result<Self, BridgeError> {
        if !bounds.valid() || !metrics.0.valid() {
            return Err(LayoutError("invalid editor geometry").into());
        }
        let (width, height) = cells(bounds.width, bounds.height, metrics)?;
        host.execute(Command::Resize { width, height })?;
        Ok(Self {
            host,
            metrics,
            bounds,
            scale: 1.,
            generation: 0,
            text_source: TextSource::KeyDown,
            anchors: BTreeMap::new(),
        })
    }
    pub fn snapshot(&self) -> Result<rune_editor::SessionSnapshot<H::Doc>, BridgeError> {
        self.host.snapshot().map_err(Into::into)
    }
    pub fn with_text_source(mut self, source: TextSource) -> Result<Self, BridgeError> {
        self.host.cancel_prefix()?;
        self.text_source = source;
        Ok(self)
    }
    /// Convenience owner-current-point insertion, not a delayed platform commit.
    pub fn commit_text(
        &mut self,
        window_id: u64,
        buffer_id: u64,
        text: String,
    ) -> Result<bool, BridgeError> {
        let snapshot = self.host.snapshot()?;
        let view = snapshot
            .views
            .iter()
            .find(|v| {
                v.id == snapshot.selected_window
                    && v.id.as_u64() == window_id
                    && v.view.buffer.as_u64() == buffer_id
            })
            .ok_or(EditorError::StaleInput)?;
        let buffer = snapshot
            .buffers
            .iter()
            .find(|b| b.id == view.view.buffer)
            .ok_or(EditorError::StaleInput)?;
        let context = CompositionContext {
            window_id,
            buffer_id,
            publication_epoch: buffer.display_snapshot().revision(),
            owner_sequence: snapshot.sequence,
            replacement: view.view.point..view.view.point,
        };
        self.commit_composition(CommittedText { context, text })
    }
    pub fn commit_composition(&mut self, committed: CommittedText) -> Result<bool, BridgeError> {
        if self.text_source != TextSource::CommittedText {
            return Err(BridgeError::UnsupportedInput);
        }
        let c = committed.context;
        let snapshot = self.host.snapshot()?;
        let view = snapshot
            .views
            .iter()
            .find(|v| v.id == snapshot.selected_window && v.id.as_u64() == c.window_id)
            .ok_or(EditorError::StaleInput)?;
        let buffer = snapshot
            .buffers
            .iter()
            .find(|b| b.id == view.view.buffer)
            .ok_or(EditorError::StaleInput)?;
        if snapshot.sequence != c.owner_sequence
            || buffer.id.as_u64() != c.buffer_id
            || buffer.display_snapshot().revision() != c.publication_epoch
            || (c.replacement.is_empty() && view.view.point != c.replacement.start)
        {
            return Err(EditorError::StaleInput.into());
        }
        // Owner validates scope, epoch, range, readonly policy and one undo entry.
        self.host.commit_replacement(
            view.id,
            buffer.id,
            c.publication_epoch,
            c.replacement,
            committed.text,
        )?;
        Ok(true)
    }
    /// Native IME mode has explicit fixed command actions, not an asynchronous
    /// guess at owner keymap-prefix consumption.
    pub fn execute_command(&mut self, command: Command) -> Result<bool, BridgeError> {
        self.host.cancel_prefix()?;
        self.host.execute(command)?;
        Ok(true)
    }
    /// Apply once. A later layout failure must retry layout, never this mutation.
    pub fn apply(&mut self, event: &InputEvent) -> Result<bool, BridgeError> {
        match event {
            InputEvent::Key { key, text, control, meta, shift } => {
                if (*control && !*meta && key == "g") || (!*control && key == "escape") {
                    self.host.cancel_prefix()?;
                    return Ok(false);
                }
                if *meta || (*control && *shift) {
                    return Err(BridgeError::UnsupportedInput);
                }
                if self.text_source == TextSource::CommittedText
                    && !*control
                    && (text.as_ref().is_some_and(|s| s.chars().all(|c| !c.is_control()))
                        || key == "enter"
                        || key == "tab")
                {
                    return Ok(false);
                }
                let k = if *control {
                    let c = one_char(key)
                        .filter(|c| c.is_ascii_alphabetic())
                        .ok_or(BridgeError::UnsupportedInput)?;
                    Key::Control(c.to_ascii_lowercase())
                } else {
                    match key.as_str() {
                        "left" if !*shift => Key::Left,
                        "right" if !*shift => Key::Right,
                        "up" if !*shift => Key::Up,
                        "down" if !*shift => Key::Down,
                        "home" if !*shift => Key::Home,
                        "end" if !*shift => Key::End,
                        "backspace" => Key::Backspace,
                        "delete" => Key::Delete,
                        "enter" => Key::Enter,
                        "tab" => Key::Char('\t'),
                        _ => Key::Char(
                            text.as_deref()
                                .and_then(one_char)
                                .filter(|c| !c.is_control())
                                .ok_or(BridgeError::UnsupportedInput)?,
                        ),
                    }
                };
                return self
                    .host
                    .dispatch_key(k)
                    .map(|d| matches!(d, Dispatch::Executed(_)))
                    .map_err(Into::into);
            }
            InputEvent::Resize { width, height, scale } => {
                if !scale.is_finite() || *scale <= 0. {
                    return Err(LayoutError("invalid editor scale").into());
                }
                if (*width, *height, *scale) == (self.bounds.width, self.bounds.height, self.scale)
                {
                    return Ok(false);
                }
                let (width_cells, height_cells) = cells(*width, *height, self.metrics)?;
                self.host
                    .execute(Command::Resize { width: width_cells, height: height_cells })?;
                self.bounds.width = *width;
                self.bounds.height = *height;
                self.scale = *scale;
            }
            InputEvent::PointerHit { hit, button: 1 } => {
                let s = self.host.snapshot()?;
                let view = s
                    .views
                    .iter()
                    .find(|v| {
                        v.id.as_u64() == hit.window_id && v.view.buffer.as_u64() == hit.buffer_id
                    })
                    .ok_or(BridgeError::StaleHit)?;
                let buffer = s
                    .buffers
                    .iter()
                    .find(|b| b.id == view.view.buffer)
                    .ok_or(BridgeError::StaleHit)?;
                if buffer.display_snapshot().revision() != hit.revision {
                    return Err(BridgeError::StaleHit);
                }
                // The owner cancels prefix atomically after successful placement.
                self.host.place_pointer(view.id, buffer.id, hit.revision, hit.position)?;
            }
            // Unscoped text, raw pointers and wheel anchoring are not retargeted.
            _ => return Err(BridgeError::UnsupportedInput),
        }
        Ok(true)
    }
    pub fn prepare_frame(&mut self) -> Result<Arc<FrameDisplay>, BridgeError> {
        let generation = self.generation.checked_add(1).ok_or(BridgeError::GenerationExhausted)?;
        let s = self.host.snapshot()?;
        let mut windows = Vec::with_capacity(s.views.len());
        let mut anchors = BTreeMap::new();
        for view in &s.views {
            let buffer = s
                .buffers
                .iter()
                .find(|b| b.id == view.view.buffer)
                .ok_or(EditorError::InvalidBuffer)?;
            let r = view.rect;
            let mut spec = WindowSpec::new(Rect {
                x: self.bounds.x + r.x as f32 * self.metrics.0.advance,
                y: self.bounds.y + r.y as f32 * self.metrics.0.line_height(),
                width: r.width as f32 * self.metrics.0.advance,
                height: r.height as f32 * self.metrics.0.line_height(),
            });
            spec.window_id = view.id.as_u64();
            spec.buffer_id = buffer.id.as_u64();
            spec.anchor = view.view.start;
            spec.point = view.view.point;
            spec.hscroll = view.view.hscroll as f32 * self.metrics.0.advance;
            let published = buffer.display_snapshot();
            if let Some(&(epoch, anchor)) = self.anchors.get(&spec.window_id) {
                if epoch == published.revision() && anchor <= published.len_chars() {
                    spec.anchor = anchor;
                }
            }
            let mut window = layout_window(&published, &spec, &[], &self.metrics)?;
            if view.id == s.selected_window && window.cursor.is_none() {
                spec.anchor = spec.point;
                window = layout_window(&published, &spec, &[], &self.metrics)?;
            }
            if view.id != s.selected_window {
                window.cursor = None;
            }
            anchors.insert(spec.window_id, (published.revision(), spec.anchor));
            windows.push(window);
        }
        let frame = Arc::new(FrameDisplay {
            protocol_version: DISPLAY_PROTOCOL_VERSION,
            generation,
            bounds: self.bounds,
            background: Color(24, 28, 36, 255),
            windows,
        });
        self.generation = generation;
        self.anchors = anchors;
        Ok(frame)
    }
}
fn one_char(s: &str) -> Option<char> {
    let mut i = s.chars();
    let c = i.next()?;
    i.next().is_none().then_some(c)
}
fn cells(width: f32, height: f32, m: FixedMetrics) -> Result<(u32, u32), BridgeError> {
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return Err(LayoutError("invalid editor resize").into());
    }
    let (w, h) = ((width / m.0.advance).floor(), (height / m.0.line_height()).floor());
    if w < 1. || h < 1. || w >= u32::MAX as f32 || h >= u32::MAX as f32 {
        return Err(LayoutError("unrepresentable editor cells").into());
    }
    Ok((w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune_editor::{BufferId, CommandOutcome, Document, RopeDocument, Session, WindowId};
    use std::{cell::Cell, ops::Range, rc::Rc};
    struct RecordingHost {
        session: Session<RopeDocument>,
        commits: Rc<Cell<usize>>,
    }
    impl EditorHost for RecordingHost {
        type Doc = RopeDocument;
        fn snapshot(&self) -> rune_editor::Result<rune_editor::SessionSnapshot<RopeDocument>> {
            Ok(self.session.snapshot())
        }
        fn execute(&mut self, c: Command) -> rune_editor::Result<CommandOutcome> {
            self.session.execute(c)
        }
        fn dispatch_key(&mut self, k: Key) -> rune_editor::Result<Dispatch> {
            self.session.dispatch(k)
        }
        fn cancel_prefix(&mut self) -> rune_editor::Result<()> {
            self.session.cancel_prefix();
            Ok(())
        }
        fn place_pointer(
            &mut self,
            w: WindowId,
            b: BufferId,
            e: u64,
            p: usize,
        ) -> rune_editor::Result<CommandOutcome> {
            self.session.place_pointer(w, b, e, p)
        }
        fn commit_replacement(
            &mut self,
            w: WindowId,
            b: BufferId,
            e: u64,
            r: Range<usize>,
            t: String,
        ) -> rune_editor::Result<CommandOutcome> {
            let out = self.session.commit_replacement(w, b, e, r, t)?;
            self.commits.set(self.commits.get() + 1);
            Ok(out)
        }
    }
    fn bounds() -> Rect {
        Rect { x: 0., y: 0., width: 640., height: 480. }
    }
    #[test]
    fn non_send_host_owns_exactly_one_insert_and_one_undo() {
        let commits = Rc::new(Cell::new(0));
        let h = RecordingHost {
            session: Session::new(RopeDocument::empty(), 80, 24).unwrap(),
            commits: commits.clone(),
        };
        let mut b = EditorBridge::new(h, FixedMetrics::default(), bounds())
            .unwrap()
            .with_text_source(TextSource::CommittedText)
            .unwrap();
        let snapshot = b.snapshot().unwrap();
        let w = snapshot.selected_window;
        let buffer = snapshot.current_buffer;
        b.commit_text(w.as_u64(), buffer.as_u64(), "λ🙂".into()).unwrap();
        assert_eq!(commits.get(), 1);
        assert_eq!(
            b.snapshot()
                .unwrap()
                .buffers
                .iter()
                .find(|b| b.id == buffer)
                .unwrap()
                .document
                .to_text(),
            "λ🙂"
        );
        b.host.execute(Command::Undo).unwrap();
        assert_eq!(
            b.snapshot()
                .unwrap()
                .buffers
                .iter()
                .find(|b| b.id == buffer)
                .unwrap()
                .document
                .to_text(),
            ""
        );
    }
    #[test]
    fn captured_scope_rejects_changed_epoch_without_an_insert() {
        let mut b = EditorBridge::new(
            Session::new(RopeDocument::empty(), 80, 24).unwrap(),
            FixedMetrics::default(),
            bounds(),
        )
        .unwrap()
        .with_text_source(TextSource::CommittedText)
        .unwrap();
        let s = b.snapshot().unwrap();
        let v = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        let buffer = s.buffers.iter().find(|x| x.id == v.view.buffer).unwrap();
        let c = CompositionContext {
            window_id: v.id.as_u64(),
            buffer_id: buffer.id.as_u64(),
            publication_epoch: buffer.display_snapshot().revision(),
            owner_sequence: s.sequence,
            replacement: 0..0,
        };
        b.commit_text(c.window_id, c.buffer_id, "first".into()).unwrap();
        assert!(matches!(
            b.commit_composition(CommittedText { context: c, text: "late".into() }),
            Err(BridgeError::Editor(EditorError::StaleInput))
        ));
        assert_eq!(b.snapshot().unwrap().buffers[0].document.to_text(), "first");
    }
    #[test]
    fn captured_composition_rejects_coalesced_buffer_aba_even_when_ids_and_text_epoch_match() {
        let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
        let original = session.current_buffer();
        let other = session.create_buffer("other", RopeDocument::empty()).unwrap();
        let mut b = EditorBridge::new(session, FixedMetrics::default(), bounds())
            .unwrap()
            .with_text_source(TextSource::CommittedText)
            .unwrap();
        let s = b.snapshot().unwrap();
        let view = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        let buffer = s.buffers.iter().find(|x| x.id == original).unwrap();
        let context = CompositionContext {
            window_id: view.id.as_u64(),
            buffer_id: original.as_u64(),
            publication_epoch: buffer.display_snapshot().revision(),
            owner_sequence: s.sequence,
            replacement: 0..0,
        };
        b.host.execute(Command::SwitchBuffer(other)).unwrap();
        b.host.execute(Command::SwitchBuffer(original)).unwrap();
        let now = b.snapshot().unwrap();
        assert_eq!(now.current_buffer, original);
        assert!(now.sequence > s.sequence);
        assert!(matches!(
            b.commit_composition(CommittedText { context, text: "late".into() }),
            Err(BridgeError::Editor(EditorError::StaleInput))
        ));
        assert_eq!(
            b.snapshot()
                .unwrap()
                .buffers
                .iter()
                .find(|b| b.id == original)
                .unwrap()
                .document
                .to_text(),
            ""
        );
    }
    #[test]
    fn captured_composition_rejects_direct_point_aba_without_text_change() {
        let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
        session.execute(Command::Insert("ab".into())).unwrap();
        let original = session.current_buffer();
        let mut b = EditorBridge::new(session, FixedMetrics::default(), bounds())
            .unwrap()
            .with_text_source(TextSource::CommittedText)
            .unwrap();
        let s = b.snapshot().unwrap();
        let view = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        let buffer = s.buffers.iter().find(|x| x.id == original).unwrap();
        let point = view.view.point;
        let context = CompositionContext {
            window_id: view.id.as_u64(),
            buffer_id: original.as_u64(),
            publication_epoch: buffer.display_snapshot().revision(),
            owner_sequence: s.sequence,
            replacement: point..point,
        };
        b.host.set_buffer_point(original, if point == 0 { 1 } else { 0 }).unwrap();
        b.host.set_buffer_point(original, point).unwrap();
        let now = b.snapshot().unwrap();
        assert!(now.sequence > s.sequence);
        assert_eq!(
            now.views.iter().find(|v| v.id == now.selected_window).unwrap().view.point,
            point
        );
        assert_eq!(
            now.buffers
                .iter()
                .find(|x| x.id == original)
                .unwrap()
                .display_snapshot()
                .revision(),
            context.publication_epoch
        );
        assert!(matches!(
            b.commit_composition(CommittedText { context, text: "late".into() }),
            Err(BridgeError::Editor(EditorError::StaleInput))
        ));
        assert_eq!(
            b.snapshot()
                .unwrap()
                .buffers
                .iter()
                .find(|x| x.id == original)
                .unwrap()
                .document
                .to_text(),
            "ab"
        );
    }
    #[test]
    fn captured_composition_rejects_direct_visible_scope_and_policy_transitions() {
        for operation in 0..3 {
            let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
            session.execute(Command::Insert("ab".into())).unwrap();
            let mut b = EditorBridge::new(session, FixedMetrics::default(), bounds())
                .unwrap()
                .with_text_source(TextSource::CommittedText)
                .unwrap();
            let s = b.snapshot().unwrap();
            let view = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
            let buffer = s.buffers.iter().find(|x| x.id == view.view.buffer).unwrap();
            let context = CompositionContext {
                window_id: view.id.as_u64(),
                buffer_id: buffer.id.as_u64(),
                publication_epoch: buffer.display_snapshot().revision(),
                owner_sequence: s.sequence,
                replacement: view.view.point..view.view.point,
            };
            match operation {
                0 => {
                    b.host.set_window_start(view.id, 1).unwrap();
                    b.host.set_window_start(view.id, view.view.start).unwrap();
                }
                1 => {
                    b.host.set_read_only(buffer.id, true).unwrap();
                    b.host.set_read_only(buffer.id, false).unwrap();
                }
                _ => {
                    let configuration = b.host.window_configuration();
                    b.host.restore_window_configuration(&configuration).unwrap();
                }
            }
            assert!(b.snapshot().unwrap().sequence > s.sequence, "operation {operation}");
            assert!(
                matches!(
                    b.commit_composition(CommittedText { context, text: "late".into() }),
                    Err(BridgeError::Editor(EditorError::StaleInput))
                ),
                "operation {operation}"
            );
            assert_eq!(
                b.snapshot()
                    .unwrap()
                    .buffers
                    .iter()
                    .find(|x| x.id == buffer.id)
                    .unwrap()
                    .document
                    .to_text(),
                "ab"
            );
        }
    }
}
