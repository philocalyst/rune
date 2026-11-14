#![cfg(feature = "rope")]
use rune_editor::{Bias, Command, Document, EditorError, MarkerDocument, RopeDocument, Session};
use rune_text::{PropSetId, Rope, Snapshot};
fn doc(text: &str) -> RopeDocument {
    RopeDocument::new(Snapshot::new(Rope::from(text)), PropSetId(0))
}

#[test]
fn pair_edits_are_native_only_and_independent_biases_follow_unicode_edits() {
    let mut s = Session::new(doc("λ🙂"), 80, 24).unwrap();
    let id = s.current_buffer();
    let before = s.snapshot();
    let pair = s.register_marker_pair(id, 1, Bias::Left, 1, Bias::Right).unwrap();
    assert_eq!(s.snapshot().sequence, before.sequence);
    assert_eq!(s.buffer_snapshot(id).unwrap().version, before.buffers[0].version);
    assert!(!s.modified(id).unwrap());
    assert_eq!(s.point(), 0);
    assert_eq!(s.execute(Command::Undo), Err(EditorError::NoUndo));
    s.execute(Command::GoTo(1)).unwrap();
    s.execute(Command::Insert("é".into())).unwrap();
    assert_eq!(s.marker_position(id, pair.0).unwrap(), Some(1));
    assert_eq!(s.marker_position(id, pair.1).unwrap(), Some(2));
    s.execute(Command::Undo).unwrap();
    assert_eq!(s.marker_position(id, pair.0).unwrap(), Some(1));
    assert_eq!(s.marker_position(id, pair.1).unwrap(), Some(1));
    s.remove_marker_pair(id, pair).unwrap();
    s.remove_marker_pair(id, pair).unwrap();
    s.execute(Command::Redo).unwrap();
    assert_eq!(s.marker_position(id, pair.0).unwrap(), None);
    assert_eq!(s.marker_position(id, pair.1).unwrap(), None);
}
#[test]
fn transfer_is_all_or_nothing_and_preserves_ids_current_buffer_and_history() {
    let mut s = Session::new(doc("abcd"), 80, 24).unwrap();
    let source = s.current_buffer();
    let target = s.create_buffer("target", doc("λ🙂")).unwrap();
    let pair = s.register_marker_pair(source, 1, Bias::Left, 3, Bias::Right).unwrap();
    let seq = s.snapshot().sequence;
    assert!(
        s.transfer_marker_pair(source, target, pair, 0, Bias::Left, 3, Bias::Right)
            .is_err()
    );
    assert_eq!(s.marker_position(source, pair.0).unwrap(), Some(1));
    assert_eq!(s.marker_position(target, pair.0).unwrap(), None);
    s.transfer_marker_pair(source, target, pair, 0, Bias::Right, 2, Bias::Left)
        .unwrap();
    assert_eq!(s.marker_position(source, pair.0).unwrap(), None);
    assert_eq!(s.marker_position(target, pair.0).unwrap(), Some(0));
    assert_eq!(s.marker_position(target, pair.1).unwrap(), Some(2));
    assert_eq!(s.current_buffer(), source);
    assert_eq!(s.snapshot().sequence, seq);
    assert!(!s.modified(source).unwrap());
    assert!(!s.modified(target).unwrap());
    assert!(s.move_marker_pair(source, pair, 0, Bias::Left, 1, Bias::Right).is_err());
}
#[test]
fn liveness_closes_only_on_committed_kill_or_session_drop() {
    let token;
    {
        let mut s = Session::new(doc("a"), 80, 24).unwrap();
        let id = s.current_buffer();
        token = s.buffer_liveness(id).unwrap();
        s.execute(Command::Insert("x".into())).unwrap();
        assert_eq!(
            s.execute(Command::KillBuffer { buffer: id, force: false }),
            Err(EditorError::ModifiedBuffer)
        );
        assert!(token.is_live());
        s.execute(Command::KillBuffer { buffer: id, force: true }).unwrap();
        assert!(!token.is_live());
        assert!(s.buffer_liveness(id).is_err());
        let replacement = s.create_buffer("recreated", doc("")).unwrap();
        assert_ne!(replacement, id);
        assert!(s.buffer_liveness(replacement).unwrap().is_live());
    }
    assert!(!token.is_live());
    let live;
    let published;
    {
        let s = Session::new(doc("retained snapshot"), 80, 24).unwrap();
        live = s.buffer_liveness(s.current_buffer()).unwrap();
        published = s.snapshot();
        assert!(live.is_live());
    }
    assert!(!live.is_live());
    assert_eq!(published.buffers[0].document.to_text(), "retained snapshot");
}

#[derive(Clone)]
struct FaultDoc {
    inner: RopeDocument,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    fail: usize,
}
impl Document for FaultDoc {
    type Error = String;
    type MarkerUndo = <RopeDocument as Document>::MarkerUndo;
    fn empty() -> Self {
        Self { inner: doc(""), calls: Default::default(), fail: 0 }
    }
    fn len_chars(&self) -> usize {
        self.inner.len_chars()
    }
    fn capture_marker_undo(&self, r: std::ops::Range<usize>) -> Result<Self::MarkerUndo, String> {
        self.inner.capture_marker_undo(r).map_err(|e| e.to_string())
    }
    fn restore_history(
        &self,
        h: &Self,
        s: Option<rune_editor::Splice>,
        r: Option<&Self::MarkerUndo>,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: self.inner.restore_history(&h.inner, s, r).map_err(|e| e.to_string())?,
            ..self.clone()
        })
    }
    fn replace(&self, r: std::ops::Range<usize>, t: &str) -> Result<Self, String> {
        Ok(Self { inner: self.inner.replace(r, t).map_err(|e| e.to_string())?, ..self.clone() })
    }
    fn visit_chars(&self, s: usize, m: usize, v: &mut dyn FnMut(usize, char) -> bool) {
        self.inner.visit_chars(s, m, v)
    }
}
impl MarkerDocument for FaultDoc {
    fn set_live_marker(&self, id: u64, p: usize, b: Bias) -> Result<Self, String> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if self.fail == usize::MAX && n == 2 { panic!("second marker panic"); }
        if n == self.fail {
            return Err("second marker fault".into());
        }
        Ok(Self {
            inner: self.inner.set_live_marker(id, p, b).map_err(|e| e.to_string())?,
            ..self.clone()
        })
    }
    fn remove_live_marker(&self, id: u64) -> Self {
        Self { inner: self.inner.remove_live_marker(id), ..self.clone() }
    }
    fn marker_position(&self, id: u64) -> Option<usize> {
        self.inner.marker_position(id)
    }
}
#[test]
fn second_marker_backend_failure_never_publishes_half_pair() {
    let d = FaultDoc { inner: doc("abc"), calls: Default::default(), fail: 2 };
    let mut s = Session::new(d, 80, 24).unwrap();
    let id = s.current_buffer();
    assert!(s.register_marker_pair(id, 0, Bias::Left, 3, Bias::Right).is_err());
    assert_eq!(s.document(id).unwrap().inner.snapshot().positions().marker_count(), 0);
    assert_eq!(s.snapshot().sequence, 0);
    assert!(!s.modified(id).unwrap());
}
#[test]
fn second_marker_transfer_failure_keeps_both_buffers_unchanged() {
    let source = FaultDoc { inner: doc("source"), calls: Default::default(), fail: 0 };
    let target = FaultDoc { inner: doc("target"), calls: Default::default(), fail: 2 };
    let mut s = Session::new(source, 80, 24).unwrap();
    let source = s.current_buffer();
    let target = s.create_buffer("target", target).unwrap();
    let pair = s.register_marker_pair(source, 1, Bias::Left, 4, Bias::Right).unwrap();
    assert!(
        s.transfer_marker_pair(source, target, pair, 0, Bias::Right, 3, Bias::Left)
            .is_err()
    );
    assert_eq!(s.marker_position(source, pair.0).unwrap(), Some(1));
    assert_eq!(s.marker_position(source, pair.1).unwrap(), Some(4));
    assert_eq!(s.document(target).unwrap().inner.snapshot().positions().marker_count(), 0);
    assert_eq!(s.snapshot().sequence, 0);
}

#[test]
fn second_marker_backend_panic_never_publishes_half_pair() {
    let d=FaultDoc{inner:doc("abc"),calls:Default::default(),fail:usize::MAX};
    let mut s=Session::new(d,80,24).unwrap();let id=s.current_buffer();
    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.register_marker_pair(id,0,Bias::Left,3,Bias::Right)
    }));
    assert!(result.is_err());
    assert_eq!(s.document(id).unwrap().inner.snapshot().positions().marker_count(),0);
    assert_eq!(s.snapshot().sequence,0);assert!(!s.modified(id).unwrap());
}
