//! Bounded UI-only composition. The virtual segment is not a document replica.
//! Rendering is an explicit floating preedit panel, never speculative reflow.
use crate::{
    composition::{CompositionError, CompositionState, CompositionToken},
    editor_transport::{CompositionAck, CompositionRequest, OwnerEvent},
    ime_context::{ContextError, PreparedImeContext},
    platform_context::{PlatformFrame, PlatformInputSnapshot, PlatformReader},
};
use rune_display::{
    FixedMetrics, FrameDisplay, Point, Rect, StringSnapshot, WindowDisplay, WindowSpec,
    layout_window,
};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug)]
pub enum PlatformError {
    Unavailable,
    Context(ContextError),
    Composition(CompositionError),
    Layout(rune_display::LayoutError),
}
impl From<ContextError> for PlatformError {
    fn from(e: ContextError) -> Self {
        Self::Context(e)
    }
}
impl From<CompositionError> for PlatformError {
    fn from(e: CompositionError) -> Self {
        Self::Composition(e)
    }
}
struct Draft {
    base: Arc<PlatformInputSnapshot>,
    rejected: bool,
    generation: u64,
    token: CompositionToken,
    original: Range<usize>,
    text: String,
    marked: Range<usize>,
    selected: Range<usize>,
    emitted: bool,
}
pub struct PlatformInputState {
    reader: PlatformReader,
    metrics: FixedMetrics,
    bundle: Option<Arc<PlatformFrame>>,
    input: Option<Arc<PlatformInputSnapshot>>,
    last_identity: Option<crate::ime_context::ImeIdentity>,
    composition: CompositionState,
    draft: Option<Draft>,
    overlay: Option<Arc<FrameDisplay>>,
    active: bool,
    after_generation: Option<u64>,
}
impl PlatformInputState {
    pub fn new(reader: PlatformReader, metrics: FixedMetrics) -> Result<Self, PlatformError> {
        if !metrics.0.valid() {
            return Err(PlatformError::Unavailable);
        }
        Ok(Self {
            reader,
            metrics,
            bundle: None,
            input: None,
            last_identity: None,
            composition: CompositionState::new()?,
            draft: None,
            overlay: None,
            active: true,
            after_generation: None,
        })
    }
    pub fn sync(&mut self) -> Option<Arc<FrameDisplay>> {
        let next = self.reader.latest()?;
        if self.bundle.as_ref().map(|b| b.sequence) != Some(next.sequence) {
            let accepted = next.input.clone().filter(|i| {
                self.active && self.after_generation.is_none_or(|g| i.generation() > g)
            });
            let changed = match (&self.input, &accepted) {
                (Some(a), Some(b)) => {
                    a.identity() != b.identity() || a.selection() != b.selection()
                }
                (None, None) => false,
                _ => true,
            };
            if changed {
                // An owner publication/tombstone is NOT an acknowledgment. Keep
                // emitted tokens until their matching ACK or explicit lifecycle cancellation.
                let lifecycle_changed = self.draft.as_ref().is_some_and(|d| {
                    accepted.as_ref().is_some_and(|i| {
                        i.identity() != d.base.identity() || i.selection() != d.base.selection()
                    })
                });
                if lifecycle_changed || !self.draft.as_ref().is_some_and(|d| d.emitted) {
                    self.cancel_draft();
                }
                // A transient tombstone is not edit success: retain the pending
                // emitted preview until ACK or an actual scope/selection transition.
            }
            if let Some(i) = &accepted {
                self.last_identity = Some(i.identity());
                self.after_generation = None;
            }
            self.input = accepted;
            self.bundle = Some(next.clone());
        }
        Some(next.frame.clone())
    }
    pub fn active(&mut self, active: bool) {
        if self.active != active {
            self.invalidate(false);
            self.active = active;
        }
        // Re-read the same bundle after focus is restored.
        if active {
            self.bundle = None;
            self.sync();
        }
    }
    pub fn invalidate(&mut self, require_new_frame: bool) {
        self.cancel_draft();
        if require_new_frame {
            self.after_generation = self.bundle.as_ref().map(|b| b.frame.generation);
        }
        self.input = None;
        self.last_identity = None;
    }
    fn cancel_draft(&mut self) {
        self.composition.invalidate();
        self.draft = None;
        self.overlay = None;
    }
    pub fn cancel_preedit(&mut self) {
        self.cancel_draft();
    }
    pub fn awaiting_ack(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| d.emitted)
    }
    /// Commands are FIFO-current-point, but a scoped IME replacement must wait
    /// for matching new geometry/epoch after navigation or an edit.
    pub fn defer_scoped_input(&mut self) {
        let identity = self.last_identity;
        self.invalidate(true);
        self.last_identity = identity;
    }
    pub fn composing(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| !d.emitted)
    }
    pub fn selected_range(&self) -> Option<Range<usize>> {
        if !self.active {
            return None;
        }
        self.draft
            .as_ref()
            .map(|d| d.selected.clone())
            .or_else(|| self.input.as_ref().map(|i| i.selection()))
    }
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.draft
            .as_ref()
            .filter(|d| !d.emitted && !d.marked.is_empty())
            .map(|d| d.marked.clone())
    }
    fn virtual_context(&self) -> Result<PreparedImeContext, PlatformError> {
        let input = self
            .draft
            .as_ref()
            .map(|d| &d.base)
            .or(self.input.as_ref())
            .ok_or(PlatformError::Unavailable)?;
        let c = input.context();
        let text = if let Some(d) = &self.draft {
            let a = byte_at(
                c.text(),
                d.original
                    .start
                    .checked_sub(c.utf16_start())
                    .ok_or(ContextError::OutsidePrepared)?,
            )?;
            let b = byte_at(
                c.text(),
                d.original
                    .end
                    .checked_sub(c.utf16_start())
                    .ok_or(ContextError::OutsidePrepared)?,
            )?;
            format!("{}{}{}", &c.text()[..a], d.text, &c.text()[b..])
        } else {
            c.text().to_owned()
        };
        Ok(PreparedImeContext::new(
            input.identity(),
            c.scalar_start(),
            c.utf16_start(),
            Arc::from(text),
        )?)
    }
    pub fn text_for_range(
        &self,
        range: Range<usize>,
    ) -> Result<(Range<usize>, String), PlatformError> {
        let c = self.virtual_context()?;
        Ok(c.read_adjusted(c.identity(), range)?)
    }
    pub fn replace_and_mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) -> Result<(), PlatformError> {
        self.edit(range, text, selected)?;
        self.rebuild_overlay();
        Ok(())
    }
    fn edit(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) -> Result<(), PlatformError> {
        if !self.active {
            return Err(PlatformError::Unavailable);
        }
        if self.draft.as_ref().is_some_and(|d| d.emitted) {
            return Err(CompositionError::Pending.into());
        }
        let input = self.input.as_ref().ok_or(PlatformError::Unavailable)?.clone();
        let virtual_context = self.virtual_context()?;
        let q = range.unwrap_or_else(|| {
            self.draft
                .as_ref()
                .map(|d| d.marked.clone())
                .unwrap_or_else(|| input.selection())
        });
        virtual_context.exact_scalar_range(input.identity(), q.clone())?;
        let inserted = text.encode_utf16().count();
        if text.len() > 65_536 || inserted > 8192 {
            return Err(ContextError::TooLarge.into());
        }
        let selected = selected.unwrap_or(inserted..inserted);
        if selected.start > selected.end || selected.end > inserted {
            return Err(ContextError::InvalidRange.into());
        }
        byte_at(text, selected.start)?;
        byte_at(text, selected.end)?;
        let original = if let Some(d) = &self.draft {
            let end = d
                .original
                .start
                .checked_add(d.text.encode_utf16().count())
                .ok_or(ContextError::Overflow)?;
            let map_start = if q.start < d.original.start {
                q.start
            } else if q.start >= end {
                d.original.end.checked_add(q.start - end).ok_or(ContextError::Overflow)?
            } else {
                d.original.start
            };
            let map_end = if q.end <= d.original.start {
                q.end
            } else if q.end > end {
                d.original.end.checked_add(q.end - end).ok_or(ContextError::Overflow)?
            } else {
                d.original.end
            };
            d.original.start.min(map_start)..d.original.end.max(map_end)
        } else {
            q.clone()
        };
        let context = input.context().replacement_context(input.identity(), original.clone())?;
        let virtual_end = if let Some(d) = &self.draft {
            original
                .end
                .checked_sub(d.original.end - d.original.start)
                .and_then(|n| n.checked_add(d.text.encode_utf16().count()))
                .ok_or(ContextError::Overflow)?
        } else {
            original.end
        };
        let start =
            byte_at(virtual_context.text(), original.start - virtual_context.utf16_start())?;
        let qa = byte_at(virtual_context.text(), q.start - virtual_context.utf16_start())?;
        let qb = byte_at(virtual_context.text(), q.end - virtual_context.utf16_start())?;
        let end = byte_at(virtual_context.text(), virtual_end - virtual_context.utf16_start())?;
        let replacement = format!(
            "{}{}{}",
            &virtual_context.text()[start..qa],
            text,
            &virtual_context.text()[qb..end]
        );
        // Validate the entire candidate before changing a token or existing draft.
        let candidate =
            format!("{}{}{}", &virtual_context.text()[..qa], text, &virtual_context.text()[qb..]);
        PreparedImeContext::new(
            input.identity(),
            input.context().scalar_start(),
            input.context().utf16_start(),
            Arc::from(candidate),
        )?;
        let marked_end = q.start.checked_add(inserted).ok_or(ContextError::Overflow)?;
        let selection = q.start.checked_add(selected.start).ok_or(ContextError::Overflow)?
            ..q.start.checked_add(selected.end).ok_or(ContextError::Overflow)?;
        let token = if let Some(d) = &self.draft {
            self.composition.amend_replacement(d.token, context.replacement)?;
            d.token
        } else {
            self.composition.set_context(Some(context))?;
            self.composition.begin()?
        };
        self.composition.preedit(token, replacement.clone())?;
        self.draft = Some(Draft {
            base: input.clone(),
            rejected: false,
            generation: input.generation(),
            token,
            original,
            text: replacement,
            marked: q.start..marked_end,
            selected: selection,
            emitted: false,
        });
        Ok(())
    }
    pub fn commit(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
    ) -> Result<OwnerEvent, PlatformError> {
        if !self.active {
            return Err(PlatformError::Unavailable);
        }
        if range.is_none()
            && (self.draft.is_none()
                || self.draft.as_ref().is_some_and(|d| d.emitted && !d.rejected))
        {
            if text.len() > 65_536 {
                return Err(ContextError::TooLarge.into());
            }
            let i = self.last_identity.ok_or(PlatformError::Unavailable)?;
            return Ok(OwnerEvent::TextInsertion {
                window_id: i.window_id,
                buffer_id: i.buffer_id,
                text: text.to_owned(),
            });
        }
        self.edit(range, text, None)?;
        self.rebuild_overlay();
        self.emit()
    }
    fn emit(&mut self) -> Result<OwnerEvent, PlatformError> {
        let d = self.draft.as_mut().ok_or(PlatformError::Unavailable)?;
        let committed = self.composition.commit(d.token, d.text.clone())?;
        d.emitted = true;
        Ok(OwnerEvent::Composition(CompositionRequest { token: d.token, committed }))
    }
    pub fn unmark(&mut self) -> Result<Option<OwnerEvent>, PlatformError> {
        if self.draft.as_ref().is_none_or(|d| d.emitted) {
            return Ok(None);
        }
        self.emit().map(Some)
    }
    pub fn acknowledge(&mut self, ack: &CompositionAck) {
        if let Some(d) = self.draft.as_mut() {
            if d.token == ack.token && ack.outcome.is_err() {
                d.rejected = true;
            }
        }
        if self.composition.acknowledge(ack.token, ack.outcome.is_ok()).is_ok()
            && ack.outcome.is_ok()
        {
            if let Some(d) = &self.draft {
                if self.input.as_ref().is_some_and(|i| i.generation() <= d.generation) {
                    self.after_generation = Some(d.generation);
                    self.input = None;
                }
            }
            self.draft = None;
            self.overlay = None;
        }
    }
    pub fn overlay(&self) -> Option<Arc<FrameDisplay>> {
        self.overlay.clone()
    }
    fn window(&self) -> Option<&WindowDisplay> {
        let i = self.input.as_ref()?.identity();
        self.bundle.as_ref()?.frame.windows.iter().find(|w| {
            w.window_id == i.window_id
                && w.buffer_id == i.buffer_id
                && w.revision == i.publication_epoch
        })
    }
    fn rebuild_overlay(&mut self) {
        self.overlay = None;
        let Some(d) = &self.draft else {
            return;
        };
        let Some(i) = &self.input else {
            return;
        };
        let Some(w) = self.window() else {
            return;
        };
        let Ok(original) = i.context().exact_scalar_range(i.identity(), d.original.clone()) else {
            return;
        };
        let Some(anchor) = scalar_bounds(w, original.start..original.start) else {
            return;
        };
        let line = self.metrics.0.line_height();
        let viewport = Rect {
            x: anchor.x,
            y: anchor.y,
            width: w.viewport.x + w.viewport.width - anchor.x,
            height: (w.viewport.y + w.viewport.height - anchor.y).min(line * 4.),
        };
        if !viewport.valid() {
            return;
        }
        let Ok(local) = local_scalar_range(
            &d.text,
            (d.selected.start - d.original.start)..(d.selected.end - d.original.start),
        ) else {
            return;
        };
        let mut spec = WindowSpec::new(viewport);
        spec.window_id = i.identity().window_id;
        spec.buffer_id = i.identity().buffer_id;
        spec.point = local.end;
        spec.selection = Some(local);
        spec.overscan_rows = 0;
        spec.scan_budget = 8192;
        let snapshot =
            StringSnapshot::new(Arc::<str>::from(d.text.as_str()), i.identity().publication_epoch);
        let Ok(panel) = layout_window(&snapshot, &spec, &[], &self.metrics) else {
            return;
        };
        let base = &self.bundle.as_ref().unwrap().frame;
        self.overlay = Some(Arc::new(FrameDisplay {
            protocol_version: base.protocol_version,
            generation: base.generation,
            bounds: viewport,
            background: panel.background,
            windows: vec![panel],
        }));
    }
    pub fn bounds_for_range(&self, range: Range<usize>) -> Option<Rect> {
        if let Some(d) = &self.draft {
            let end = d.original.start.checked_add(d.text.encode_utf16().count())?;
            if range.start >= d.original.start && range.end <= end {
                let local = local_scalar_range(
                    &d.text,
                    range.start - d.original.start..range.end - d.original.start,
                )
                .ok()?;
                return scalar_bounds(self.overlay.as_ref()?.windows.first()?, local);
            }
            // Avoid claiming original geometry is virtual reflow.
            return None;
        }
        let i = self.input.as_ref()?;
        scalar_bounds(self.window()?, i.context().exact_scalar_range(i.identity(), range).ok()?)
    }
    pub fn character_index_for_point(&self, point: Point) -> Option<usize> {
        if let Some(d) = &self.draft {
            let hit = self.overlay.as_ref()?.hit_test(point)?;
            return d
                .original
                .start
                .checked_add(d.text.chars().take(hit.position).map(char::len_utf16).sum());
        }
        let i = self.input.as_ref()?;
        let hit = self.bundle.as_ref()?.frame.hit_test(point)?;
        if (hit.window_id, hit.buffer_id, hit.revision)
            != (i.identity().window_id, i.identity().buffer_id, i.identity().publication_epoch)
        {
            return None;
        }
        i.context().scalar_to_utf16(i.identity(), hit.position).ok()
    }
}
fn byte_at(text: &str, utf16: usize) -> Result<usize, ContextError> {
    let mut n = 0;
    for (byte, ch) in text.char_indices() {
        if n == utf16 {
            return Ok(byte);
        }
        n += ch.len_utf16();
        if n > utf16 {
            return Err(ContextError::NotScalarBoundary);
        }
    }
    if n == utf16 { Ok(text.len()) } else { Err(ContextError::OutsidePrepared) }
}
fn local_scalar_range(text: &str, range: Range<usize>) -> Result<Range<usize>, ContextError> {
    if range.start > range.end {
        return Err(ContextError::InvalidRange);
    }
    Ok(text[..byte_at(text, range.start)?].chars().count()
        ..text[..byte_at(text, range.end)?].chars().count())
}
fn scalar_bounds(w: &WindowDisplay, range: Range<usize>) -> Option<Rect> {
    if range.start > range.end {
        return None;
    }
    if range.is_empty() {
        if let Some(c) = &w.cursor {
            if c.position == range.start {
                return c.bounds.intersection(w.viewport);
            }
        }
        for row in &w.rows {
            for c in &row.clusters {
                if c.source.start == range.start || c.source.end == range.start {
                    let x = if c.source.start == range.start {
                        c.bounds.x
                    } else {
                        c.bounds.x + c.bounds.width
                    };
                    return Rect { x, y: c.bounds.y, width: 1., height: c.bounds.height }
                        .intersection(w.viewport);
                }
            }
        }
        return None;
    }
    // Only fully represented single-row ranges have an unambiguous rectangle.
    for row in &w.rows {
        let first = row.clusters.iter().find(|c| c.source.start == range.start);
        let last = row.clusters.iter().find(|c| c.source.end == range.end);
        if let (Some(a), Some(b)) = (first, last) {
            return Rect {
                x: a.bounds.x,
                y: a.bounds.y,
                width: b.bounds.x + b.bounds.width - a.bounds.x,
                height: a.bounds.height,
            }
            .intersection(w.viewport);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        editor_bridge::EditorBridge,
        platform_context::{PlatformPublisher, platform_frames, prepare},
    };
    use rune_editor::{Command, Document, RopeDocument, Session};
    fn fixture() -> (PlatformInputState, PlatformPublisher) {
        let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
        session.execute(Command::Insert("a🙂bc".into())).unwrap();
        session.execute(Command::GoTo(2)).unwrap();
        let mut bridge = EditorBridge::new(
            session,
            FixedMetrics::default(),
            Rect { x: 0., y: 0., width: 640., height: 480. },
        )
        .unwrap();
        let frame = bridge.prepare_frame().unwrap();
        let input = prepare(&bridge, &frame).unwrap();
        let (publisher, reader) = platform_frames();
        publisher.publish(frame, input).unwrap();
        let mut state = PlatformInputState::new(reader, FixedMetrics::default()).unwrap();
        state.sync();
        (state, publisher)
    }
    #[test]
    fn virtual_preedit_and_expanded_replacement_commit_once() {
        let (mut s, _) = fixture();
        s.replace_and_mark(None, "候🙂", Some(1..3)).unwrap();
        assert_eq!(s.marked_range(), Some(3..6));
        assert_eq!(s.selected_range(), Some(4..6));
        assert_eq!(s.text_for_range(0..8).unwrap().1, "a🙂候🙂bc");
        s.replace_and_mark(Some(1..4), "X", None).unwrap();
        assert_eq!(s.text_for_range(0..6).unwrap().1, "aX🙂bc");
        let OwnerEvent::Composition(request) = s.unmark().unwrap().unwrap() else {
            panic!("scoped composition expected")
        };
        assert_eq!(request.committed.context.replacement, 1..2);
        assert_eq!(request.committed.text, "X🙂");
        assert!(s.unmark().unwrap().is_none());
        assert!(s.awaiting_ack());
        assert!(s.overlay().is_some());
        s.acknowledge(&CompositionAck {
            token: request.token,
            outcome: Err(crate::editor_bridge::BridgeError::StaleHit),
        });
        assert!(s.awaiting_ack());
        assert!(s.commit(None, "must resolve rejected edit").is_err());
        s.acknowledge(&CompositionAck { token: request.token, outcome: Ok(()) });
        assert!(!s.awaiting_ack());
    }
    #[test]
    fn surrogate_interior_and_oversize_fail_without_changing_draft() {
        let (mut s, _) = fixture();
        assert!(s.replace_and_mark(Some(2..3), "x", None).is_err());
        assert!(!s.composing());
        s.replace_and_mark(None, "🙂", None).unwrap();
        assert!(s.replace_and_mark(None, "z", Some(2..2)).is_err());
        assert!(s.replace_and_mark(None, &"x".repeat(8193), None).is_err());
        assert_eq!(s.marked_range(), Some(3..5));
        assert_eq!(s.text_for_range(4..4).unwrap(), (3..5, "🙂".into()));
    }
    #[test]
    fn tombstone_is_not_ack_and_late_ack_cannot_clear_new_draft() {
        let (mut s, publisher) = fixture();
        let OwnerEvent::Composition(first) = s.commit(Some(1..3), "x").unwrap() else { panic!() };
        publisher.invalidate().unwrap();
        s.sync();
        assert!(s.awaiting_ack());
        assert!(s.selected_range().is_some());
        assert!(s.text_for_range(0..4).is_ok());
        s.acknowledge(&CompositionAck { token: first.token, outcome: Ok(()) });
        assert!(!s.awaiting_ack());
        // Plain typing can queue against the last selected identity, but scoped
        // replacement cannot reuse a tombstoned publication.
        assert!(matches!(s.commit(None, "y").unwrap(), OwnerEvent::TextInsertion { .. }));
        assert!(s.commit(Some(1..1), "z").is_err());
        let (mut fresh, _) = fixture();
        fresh.replace_and_mark(None, "new", None).unwrap();
        fresh.acknowledge(&CompositionAck { token: first.token, outcome: Ok(()) });
        assert!(fresh.composing());
    }
    #[test]
    fn cancel_and_geometry_never_fabricate_offscreen_origin() {
        let (mut s, _) = fixture();
        assert!(s.bounds_for_range(2..2).is_none());
        assert!(s.bounds_for_range(900..900).is_none());
        assert!(s.bounds_for_range(3..3).is_some());
        s.replace_and_mark(None, "候補", None).unwrap();
        assert!(s.overlay().is_some());
        assert!(s.bounds_for_range(3..5).is_some());
        assert!(s.bounds_for_range(0..1).is_none());
        s.active(false);
        assert!(s.unmark().unwrap().is_none());
        assert!(s.commit(None, "lost").is_err());
    }
    #[test]
    fn full_preedit_owner_publication_ack_and_one_undo_pipeline() {
        use crate::editor_transport::pending_editor_transport;
        use rune_display::host_updates;
        let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
        session.execute(Command::Insert("a🙂b".into())).unwrap();
        session.execute(Command::GoTo(2)).unwrap();
        let bridge = EditorBridge::new(
            session,
            FixedMetrics::default(),
            Rect { x: 0., y: 0., width: 640., height: 480. },
        )
        .unwrap();
        let (publisher, _updates) = host_updates();
        let (platform, reader) = platform_frames();
        let (sender, mut replies, owner) = pending_editor_transport(4, publisher);
        let mut worker = owner.bind_ime_initialized(bridge, platform).unwrap();
        let mut ui = PlatformInputState::new(reader, FixedMetrics::default()).unwrap();
        ui.sync();
        ui.replace_and_mark(Some(1..3), "候🙂", Some(1..3)).unwrap();
        assert!(ui.overlay().is_some());
        let event = ui.unmark().unwrap().unwrap();
        assert!(ui.awaiting_ack());
        sender.try_send(event).unwrap();
        assert!(worker.try_step().unwrap().edit.unwrap());
        let ack = replies.take().unwrap();
        assert!(ack.outcome.is_ok());
        ui.acknowledge(&ack);
        ui.sync();
        assert!(!ui.awaiting_ack());
        assert_eq!(ui.selected_range(), Some(4..4));
        assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "a候🙂b");
        assert!(replies.take().is_none());
        sender.try_send(OwnerEvent::Command(Command::Undo)).unwrap();
        worker.try_step().unwrap().edit.unwrap();
        ui.sync();
        assert_eq!(worker.bridge().snapshot().unwrap().buffers[0].document.to_text(), "a🙂b");
    }
    #[test]
    fn construction_during_startup_tombstone_keeps_frame_disabled_then_recovers() {
        let (state, publisher) = fixture();
        let saved = state.bundle.as_ref().unwrap().clone();
        publisher.invalidate().unwrap();
        let mut startup =
            PlatformInputState::new(state.reader.clone(), FixedMetrics::default()).unwrap();
        assert!(startup.sync().is_some());
        assert!(startup.selected_range().is_none());
        assert!(startup.commit(Some(0..0), "stale").is_err());
        assert!(startup.marked_range().is_none());
        publisher
            .publish(saved.frame.clone(), (**saved.input.as_ref().unwrap()).clone())
            .unwrap();
        assert!(startup.sync().is_some());
        assert!(startup.selected_range().is_some());
    }
}
