//! Atomic frame + bounded text-context publication. Only plain prepared Unicode
//! crosses to the UI; no document handle, Context or property lease crosses here.
use crate::{
    editor_bridge::{BridgeError, EditorBridge},
    ime_context::{ImeIdentity, PreparedImeContext},
};
use rune_display::{FrameDisplay, TextSnapshot};
use rune_editor::{DisplayDocument, Document, EditorHost, PlatformTextDocument};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct PlatformInputSnapshot {
    pub(crate) generation: u64,
    pub(crate) identity: ImeIdentity,
    pub(crate) context: Arc<PreparedImeContext>,
    pub(crate) selection: Range<usize>,
}
impl PlatformInputSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn identity(&self) -> ImeIdentity {
        self.identity
    }
    pub fn context(&self) -> &PreparedImeContext {
        &self.context
    }
    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }
}
#[derive(Clone)]
pub struct PlatformFrame {
    pub frame: Arc<FrameDisplay>,
    pub input: Option<Arc<PlatformInputSnapshot>>,
    pub sequence: u64,
}
struct State {
    sequence: u64,
    latest: Option<Arc<PlatformFrame>>,
}
pub struct PlatformPublisher(Arc<Mutex<State>>);
#[derive(Clone)]
pub struct PlatformReader(Arc<Mutex<State>>);
pub fn platform_frames() -> (PlatformPublisher, PlatformReader) {
    let state = Arc::new(Mutex::new(State { sequence: 0, latest: None }));
    (PlatformPublisher(state.clone()), PlatformReader(state))
}
impl PlatformPublisher {
    pub(crate) fn publish(
        &self,
        frame: Arc<FrameDisplay>,
        input: PlatformInputSnapshot,
    ) -> Result<(), BridgeError> {
        if input.generation != frame.generation {
            return Err(BridgeError::Publish("IME/frame generation mismatch"));
        }
        let mut state = self.0.lock().map_err(|_| BridgeError::Publish("IME mailbox poisoned"))?;
        state.sequence = state.sequence.checked_add(1).ok_or(BridgeError::GenerationExhausted)?;
        state.latest = Some(Arc::new(PlatformFrame {
            frame,
            input: Some(Arc::new(input)),
            sequence: state.sequence,
        }));
        Ok(())
    }
    pub(crate) fn invalidate(&self) -> Result<(), BridgeError> {
        let mut state = self.0.lock().map_err(|_| BridgeError::Publish("IME mailbox poisoned"))?;
        if let Some(old) = state.latest.clone() {
            state.sequence =
                state.sequence.checked_add(1).ok_or(BridgeError::GenerationExhausted)?;
            state.latest = Some(Arc::new(PlatformFrame {
                frame: old.frame.clone(),
                input: None,
                sequence: state.sequence,
            }));
        }
        Ok(())
    }
}
impl PlatformReader {
    pub fn latest(&self) -> Option<Arc<PlatformFrame>> {
        self.0.lock().ok()?.latest.clone()
    }
}
pub(crate) fn prepare<H: EditorHost>(
    bridge: &EditorBridge<H>,
    frame: &FrameDisplay,
) -> Result<PlatformInputSnapshot, BridgeError>
where
    H::Doc: PlatformTextDocument,
{
    let snapshot = bridge.snapshot()?;
    let view = snapshot
        .views
        .iter()
        .find(|v| v.id == snapshot.selected_window)
        .ok_or(rune_editor::EditorError::InvalidWindow)?;
    let buffer = snapshot
        .buffers
        .iter()
        .find(|b| b.id == view.view.buffer)
        .ok_or(rune_editor::EditorError::InvalidBuffer)?;
    let epoch = buffer.display_snapshot().revision();
    let identity = ImeIdentity {
        window_id: view.id.as_u64(),
        buffer_id: buffer.id.as_u64(),
        publication_epoch: epoch,
        owner_sequence: snapshot.sequence,
    };
    if !frame.windows.iter().any(|w| {
        w.window_id == identity.window_id
            && w.buffer_id == identity.buffer_id
            && w.revision == epoch
            && w.cursor.as_ref().is_some_and(|c| c.position == view.view.point)
    }) {
        return Err(BridgeError::Layout(rune_display::LayoutError(
            "IME context does not match prepared caret/frame",
        )));
    }
    let point = view.view.point;
    let start = point.saturating_sub(512);
    let end = point.saturating_add(512).min(buffer.document.len_chars());
    let origin = buffer.document.char_to_utf16(start)?;
    let selected = buffer.document.char_to_utf16(point)?;
    if buffer.document.utf16_to_char(selected)? != point {
        return Err(BridgeError::Layout(rune_display::LayoutError(
            "inconsistent Unicode text index",
        )));
    }
    let text: String = buffer.document.chars_from(start).take(end.saturating_sub(start)).collect();
    let context =
        PreparedImeContext::new(identity, start, origin, Arc::from(text)).map_err(|_| {
            BridgeError::Layout(rune_display::LayoutError("invalid bounded IME context"))
        })?;
    if context.scalar_to_utf16(identity, point).ok() != Some(selected) {
        return Err(BridgeError::Layout(rune_display::LayoutError("IME origin/index mismatch")));
    }
    Ok(PlatformInputSnapshot {
        generation: frame.generation,
        identity,
        context: Arc::new(context),
        selection: selected..selected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune_display::{FixedMetrics, Rect};
    use rune_editor::{Command, Document, RopeDocument, Session};
    fn bridge() -> EditorBridge<Session<RopeDocument>> {
        let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
        session.execute(Command::Insert("é🙂x".into())).unwrap();
        session.execute(Command::GoTo(2)).unwrap();
        EditorBridge::new(
            session,
            FixedMetrics::default(),
            Rect { x: 0., y: 0., width: 640., height: 480. },
        )
        .unwrap()
    }
    #[test]
    fn prepared_unicode_origin_selection_and_frame_share_one_epoch() {
        let mut b = bridge();
        let frame = b.prepare_frame().unwrap();
        let input = prepare(&b, &frame).unwrap();
        assert_eq!(input.selection, 3..3);
        assert_eq!(input.context.scalar_to_utf16(input.identity, 2), Ok(3));
        assert_eq!(input.context.exact_scalar_range(input.identity, 1..3), Ok(1..2));
        let mut bad = (*frame).clone();
        bad.windows[0].revision += 1;
        assert!(prepare(&b, &bad).is_err());
    }
    #[test]
    fn invalidation_is_atomic_with_retained_frame_not_stale_writable_context() {
        let mut b = bridge();
        let frame = b.prepare_frame().unwrap();
        let input = prepare(&b, &frame).unwrap();
        let (p, r) = platform_frames();
        p.publish(frame.clone(), input).unwrap();
        let old = r.latest().unwrap();
        p.invalidate().unwrap();
        let new = r.latest().unwrap();
        assert_eq!(old.frame.generation, new.frame.generation);
        assert!(old.input.is_some());
        assert!(new.input.is_none());
        assert!(new.sequence > old.sequence);
    }
}
