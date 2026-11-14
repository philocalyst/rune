//! Staged runtime-owned editor root. This is NOT a live legacy Lisp binding.
use crate::core::{
    gc::Context,
    object::{Object, PropertyRegistry, PropertySetHandle, RuntimePropertyLease},
};
use rune_editor::{Bias, DisplayDocument, Document, MarkerDocument, MetadataDocument, Splice};
use rune_text::{CharPos, DeletionMarkers, PropSetId, Rope, Snapshot};
use std::{ops::Range, sync::Arc};
#[derive(Clone, Debug)]
struct Binding {
    lease: RuntimePropertyLease,
    insertion: PropertySetHandle,
}
/// Only this module pairs text/property IDs with an authenticated registry lease.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeDocument {
    snapshot: Snapshot,
    lineage: Arc<()>,
    binding: Option<Binding>,
}
impl RuntimeDocument {
    fn new(text: &str, registry: &PropertyRegistry, cx: &Context) -> anyhow::Result<Self> {
        let insertion = registry.empty(cx)?;
        let lease = registry.prepare_lease([], &insertion, [], [], cx)?;
        Ok(Self {
            snapshot: Snapshot::new(Rope::from(text)),
            lineage: Arc::new(()),
            binding: Some(Binding { lease, insertion }),
        })
    }
    fn set_properties(
        &self,
        range: Range<CharPos>,
        handle: &PropertySetHandle,
        registry: &PropertyRegistry,
        cx: &Context,
    ) -> anyhow::Result<Self> {
        let insertion = match &self.binding {
            Some(b) => b.insertion.clone(),
            None => registry.empty(cx)?,
        };
        let snapshot = self.snapshot.set_properties(range, PropSetId(handle.id()))?;
        let lease = registry.prepare_lease(
            snapshot.properties().runs().map(|(_, id)| id.0),
            &insertion,
            self.binding.iter().map(|b| &b.lease),
            [handle],
            cx,
        )?;
        Ok(Self {
            snapshot,
            lineage: self.lineage.clone(),
            binding: Some(Binding { lease, insertion }),
        })
    }
    fn resolve<'a>(&self, id: u32, cx: &'a Context) -> anyhow::Result<Object<'a>> {
        self.binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Unbound nil-only document"))?
            .lease
            .resolve(id, cx)
    }
}
impl Document for RuntimeDocument {
    type Error = anyhow::Error;
    type MarkerUndo = DeletionMarkers;
    fn empty() -> Self {
        Self { snapshot: Snapshot::new(Rope::from("")), lineage: Arc::new(()), binding: None }
    }
    fn len_chars(&self) -> usize {
        self.snapshot.len_chars()
    }
    fn capture_marker_undo(&self, r: Range<usize>) -> anyhow::Result<DeletionMarkers> {
        Ok(DeletionMarkers::capture(
            self.snapshot.positions(),
            CharPos(r.start)..CharPos(r.end),
        )?)
    }
    fn restore_history(
        &self,
        h: &Self,
        s: Option<Splice>,
        records: Option<&DeletionMarkers>,
    ) -> anyhow::Result<Self> {
        if let (Some(c), Some(old)) = (&self.binding, &h.binding) {
            old.lease.retain_for_snapshot([], &c.insertion)?;
        }
        let delta = s
            .map(|s| rune_text::EditDelta::new(CharPos(s.start)..CharPos(s.end), s.inserted))
            .transpose()?;
        let snapshot = match (delta.as_ref(), records) {
            (Some(d), Some(r)) => {
                self.snapshot.restore_content_with_marker_undo(&h.snapshot, d, r)?
            }
            _ => self.snapshot.restore_content(&h.snapshot, delta.as_ref())?,
        };
        let binding = h
            .binding
            .as_ref()
            .or(self.binding.as_ref())
            .map(|b| {
                let insertion = self.binding.as_ref().map_or(&b.insertion, |c| &c.insertion);
                Ok::<_, anyhow::Error>(Binding {
                    lease: b.lease.retain_for_snapshot(
                        snapshot.properties().runs().map(|(_, id)| id.0),
                        insertion,
                    )?,
                    insertion: insertion.clone(),
                })
            })
            .transpose()?;
        Ok(Self { snapshot, lineage: h.lineage.clone(), binding })
    }
    fn replace(&self, r: Range<usize>, text: &str) -> anyhow::Result<Self> {
        let id = self.binding.as_ref().map_or(0, |b| b.insertion.id());
        let (snapshot, _) =
            self.snapshot.edit(CharPos(r.start)..CharPos(r.end), text, PropSetId(id))?;
        let binding = self
            .binding
            .as_ref()
            .map(|b| {
                Ok::<_, anyhow::Error>(Binding {
                    lease: b.lease.retain_for_snapshot(
                        snapshot.properties().runs().map(|(_, id)| id.0),
                        &b.insertion,
                    )?,
                    insertion: b.insertion.clone(),
                })
            })
            .transpose()?;
        Ok(Self { snapshot, lineage: Arc::new(()), binding })
    }
    fn visit_chars(&self, start: usize, max: usize, visitor: &mut dyn FnMut(usize, char) -> bool) {
        if let Ok(chars) = self.snapshot.chars_from(CharPos(start)) {
            for (i, c) in chars.take(max).enumerate() {
                if !visitor(start + i, c) {
                    break;
                }
            }
        }
    }
}
impl MetadataDocument for RuntimeDocument {
    fn shares_text_lineage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lineage, &other.lineage)
    }
}
impl DisplayDocument for RuntimeDocument {
    fn chars_from(&self, start: usize) -> Box<dyn Iterator<Item = char> + '_> {
        Box::new(
            self.snapshot
                .chars_from(CharPos(start.min(self.len_chars())))
                .expect("clamped position"),
        )
    }
    fn next_line_start(&self, start: usize) -> Option<usize> {
        self.snapshot.next_line_start(CharPos(start)).map(|p| p.0)
    }
}
impl MarkerDocument for RuntimeDocument {
    fn set_live_marker(&self, id: u64, p: usize, b: Bias) -> anyhow::Result<Self> {
        let b = match b {
            Bias::Left => rune_text::Bias::Left,
            Bias::Right => rune_text::Bias::Right,
        };
        Ok(Self {
            snapshot: self.snapshot.with_marker(rune_text::MarkerId(id), CharPos(p), b)?,
            lineage: self.lineage.clone(),
            binding: self.binding.clone(),
        })
    }
    fn remove_live_marker(&self, id: u64) -> Self {
        Self {
            snapshot: self.snapshot.without_marker(rune_text::MarkerId(id)),
            lineage: self.lineage.clone(),
            binding: self.binding.clone(),
        }
    }
    fn marker_position(&self, id: u64) -> Option<usize> {
        self.snapshot.positions().get(rune_text::MarkerId(id)).map(|(p, _)| p.0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, Session};
    #[test]
    fn metadata_redo_history_alone_retains_gc_property() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let registry = PropertyRegistry::new(cx);
        let mut session =
            Session::new(RuntimeDocument::new("λ🙂", &registry, cx).unwrap(), 80, 24).unwrap();
        let buffer = session.current_buffer();
        // SAFETY: fresh scalar string allocated in this exact owning Context.
        let handle = unsafe { registry.intern(cx.add("face"), cx) }.unwrap();
        let id = handle.id();
        let captured = session.buffer_snapshot(buffer).unwrap();
        let prepared = captured
            .document
            .set_properties(CharPos(0)..CharPos(2), &handle, &registry, cx)
            .unwrap();
        session.publish_metadata(buffer, captured.version, prepared).unwrap();
        drop(handle);
        drop(captured);
        assert_eq!(session.document(buffer).unwrap().resolve(id, cx).unwrap(), "face");
        session.execute(Command::Undo).unwrap();
        cx.garbage_collect(true);
        assert!(session.document(buffer).unwrap().resolve(id, cx).is_err());
        session.execute(Command::Redo).unwrap();
        assert_eq!(session.document(buffer).unwrap().resolve(id, cx).unwrap(), "face");
    }
    #[test]
    fn history_rejects_distinct_registry_even_nil_only_same_context() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let a = PropertyRegistry::new(&cx);
        let b = PropertyRegistry::new(&cx);
        let current = RuntimeDocument::new("x", &a, &cx).unwrap();
        let other = RuntimeDocument::new("x", &b, &cx).unwrap();
        assert!(current.restore_history(&other, None, None).is_err());
    }
}

/// Sole mutable session and property allocator, confined to its Context owner.
pub(crate) struct RuntimeEditor {
    session: rune_editor::Session<RuntimeDocument>,
    properties: PropertyRegistry,
}
pub(crate) struct PreparedMetadata {
    buffer: rune_editor::BufferId,
    epoch: u64,
    document: RuntimeDocument,
}
impl RuntimeEditor {
    pub(crate) fn new(cx: &Context, width: u32, height: u32) -> anyhow::Result<Self> {
        let properties = PropertyRegistry::new(cx);
        let session =
            rune_editor::Session::new(RuntimeDocument::new("", &properties, cx)?, width, height)?;
        Ok(Self { session, properties })
    }
    pub(crate) fn session(&self) -> &rune_editor::Session<RuntimeDocument> {
        &self.session
    }
    pub(crate) fn create_buffer(
        &mut self,
        name: &str,
        text: &str,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::BufferId> {
        self.properties.empty(cx)?;
        Ok(self
            .session
            .create_buffer(name, RuntimeDocument::new(text, &self.properties, cx)?)?)
    }
    /// Caller must ensure every reachable local Lisp value belongs to cx (or
    /// audited global storage) and is rooted across allocation. Registry checks
    /// authenticate the destination, NOT arbitrary payload graph provenance.
    pub(crate) unsafe fn intern_property_set(
        &self,
        value: Object,
        cx: &Context,
    ) -> anyhow::Result<PropertySetHandle> {
        // SAFETY: forwarded caller precondition above.
        unsafe { self.properties.intern(value, cx) }
    }
    pub(crate) fn prepare_property_set(
        &self,
        buffer: rune_editor::BufferId,
        range: Range<CharPos>,
        handle: &PropertySetHandle,
        cx: &Context,
    ) -> anyhow::Result<PreparedMetadata> {
        self.properties.empty(cx)?;
        let captured = self.session.buffer_snapshot(buffer)?;
        let document = captured.document.set_properties(range, handle, &self.properties, cx)?;
        Ok(PreparedMetadata { buffer, epoch: captured.version, document })
    }
    pub(crate) fn publish_property_set(
        &mut self,
        prepared: PreparedMetadata,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::CommandOutcome> {
        self.properties.empty(cx)?;
        Ok(self
            .session
            .publish_metadata(prepared.buffer, prepared.epoch, prepared.document)?)
    }
}
/// Borrowed host; never Send a live Context or extract a second mutable Session.
pub(crate) struct RuntimeEditorHost<'a, 'roots> {
    editor: &'a mut RuntimeEditor,
    cx: &'a mut Context<'roots>,
}
impl rune_editor::EditorHost for RuntimeEditorHost<'_, '_> {
    type Doc = RuntimeDocument;
    fn session(&self) -> &rune_editor::Session<RuntimeDocument> {
        self.editor.session()
    }
    fn execute(
        &mut self,
        c: rune_editor::Command,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        self.editor.session.execute(c)
    }
    fn dispatch_key(&mut self, k: rune_editor::Key) -> rune_editor::Result<rune_editor::Dispatch> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        self.editor.session.dispatch(k)
    }
    fn cancel_prefix(&mut self) {
        self.editor.session.cancel_prefix()
    }
    fn place_pointer(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        p: usize,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        self.editor.session.place_pointer(w, b, e, p)
    }
    fn commit_replacement(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        r: Range<usize>,
        t: String,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        self.editor.session.commit_replacement(w, b, e, r, t)
    }
}
/// Call inside the dedicated owner thread. Only channels/configuration cross
/// into that thread; the higher-ranked callback cannot return a borrowed host.
/// This establishes ownership, NOT live legacy Lisp primitive bindings.
pub(crate) fn with_runtime_owner<R>(
    width: u32,
    height: u32,
    run: impl for<'a, 'roots> FnOnce(RuntimeEditorHost<'a, 'roots>) -> R,
) -> anyhow::Result<R> {
    let roots = crate::core::gc::RootSet::default();
    let mut cx = Context::new(&roots);
    let mut editor = RuntimeEditor::new(&cx, width, height)?;
    Ok(run(RuntimeEditorHost { editor: &mut editor, cx: &mut cx }))
}
/// Callback startup errors must reach the UI's explicit failure/rejection path.
/// This factory owns no transport and cannot acknowledge undelivered input.
pub(crate) fn try_with_runtime_owner<R>(
    width: u32,
    height: u32,
    run: impl for<'a, 'roots> FnOnce(RuntimeEditorHost<'a, 'roots>) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    with_runtime_owner(width, height, run)?
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn owner_thread_host_scoped_input_is_single_undo_and_stale_replay_rejected() {
        let text = std::thread::spawn(|| {
            try_with_runtime_owner(80, 24, |mut host| {
                let b = host.session().current_buffer();
                let w = host.session().selected_window();
                let e = host.session().buffer_snapshot(b)?.version;
                host.commit_replacement(w, b, e, 0..0, "λ🙂".into())?;
                assert!(host.commit_replacement(w, b, e, 0..0, "duplicate".into()).is_err());
                host.execute(Command::Undo)?;
                assert_eq!(host.session().document(b)?.to_text(), "");
                host.execute(Command::Redo)?;
                Ok(host.session().document(b)?.to_text())
            })
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(text, "λ🙂");
    }
    #[test]
    fn startup_error_propagates_without_fabricated_success() {
        let error = try_with_runtime_owner(80, 24, |_| -> anyhow::Result<()> {
            anyhow::bail!("initial frame unavailable")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "initial frame unavailable");
    }
}
impl RuntimeEditor {
    pub(crate) fn set_current_buffer(
        &mut self,
        id: rune_editor::BufferId,
        cx: &Context,
    ) -> anyhow::Result<()> {
        self.properties.empty(cx)?;
        Ok(self.session.set_current_buffer(id)?)
    }
    pub(crate) fn current_lisp_point(&self, cx: &Context) -> anyhow::Result<i64> {
        self.properties.empty(cx)?;
        Ok(rune_editor::to_lisp_point(
            self.session.current_point()?,
            self.session.document(self.session.current_buffer())?.len_chars(),
        )?)
    }
    pub(crate) fn set_lisp_point(&mut self, point: i64, cx: &Context) -> anyhow::Result<()> {
        self.properties.empty(cx)?;
        let position = rune_editor::from_lisp_point(
            point,
            self.session.document(self.session.current_buffer())?.len_chars(),
        )?;
        Ok(self.session.set_current_point(position)?)
    }
    pub(crate) fn replace_current_lisp_range(
        &mut self,
        start: i64,
        end: i64,
        text: &str,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::CommandOutcome> {
        self.properties.empty(cx)?;
        let length = self.session.document(self.session.current_buffer())?.len_chars();
        let start = rune_editor::from_lisp_point(start, length)?;
        let end = rune_editor::from_lisp_point(end, length)?;
        Ok(self.session.replace_current_buffer(start..end, text)?)
    }
}
#[cfg(test)]
mod current_buffer_tests {
    use super::*;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn background_runtime_edit_does_not_redirect_native_input() {
        try_with_runtime_owner(80, 24, |mut host| {
            let selected = host.session().current_buffer();
            let background = host.editor.create_buffer("background", "λ🙂", host.cx)?;
            host.editor.set_current_buffer(background, host.cx)?;
            host.editor.set_lisp_point(2, host.cx)?;
            assert_eq!(host.editor.current_lisp_point(host.cx)?, 2);
            host.editor.replace_current_lisp_range(2, 3, "界", host.cx)?;
            assert_eq!(host.session().document(background)?.to_text(), "λ界");
            assert_eq!(host.session().document(selected)?.to_text(), "");
            assert!(host.editor.set_lisp_point(0, host.cx).is_err());
            host.execute(Command::Insert("native".into()))?;
            assert_eq!(host.session().current_buffer(), selected);
            assert_eq!(host.session().document(selected)?.to_text(), "native");
            host.execute(Command::Undo)?;
            assert_eq!(host.session().document(selected)?.to_text(), "");
            assert_eq!(host.session().document(background)?.to_text(), "λ界");
            Ok(())
        })
        .unwrap();
    }
}
