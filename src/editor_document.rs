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
    #[cfg(test)]
    pub(crate) fn marker_count(&self) -> usize {
        self.snapshot.positions().marker_count()
    }
    pub(crate) fn text(&self) -> &Rope {
        self.snapshot.text()
    }
    pub(crate) fn properties(&self) -> &rune_text::PropRuns {
        self.snapshot.properties()
    }
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
    pub(crate) fn resolve<'a>(&self, id: u32, cx: &'a Context) -> anyhow::Result<Object<'a>> {
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
    fn insert_before_markers(&self, at:usize, text:&str)->anyhow::Result<Self> {
        let id=self.binding.as_ref().map_or(0,|b|b.insertion.id());
        let inserted=Snapshot::new(rune_text::Rope::from(text)).set_properties(CharPos(0)..CharPos(text.chars().count()),PropSetId(id))?;
        let (snapshot,_)=self.snapshot.insert_before_markers(CharPos(at),&inserted)?;
        let binding=self.binding.as_ref().map(|b| Ok::<_,anyhow::Error>(Binding{
            lease:b.lease.retain_for_snapshot(snapshot.properties().runs().map(|(_,id)|id.0),&b.insertion)?,insertion:b.insertion.clone()
        })).transpose()?;
        Ok(Self{snapshot,lineage:Arc::new(()),binding})
    }
    fn replace_preserving_properties(
        &self,
        range: Range<usize>,
        text: &str,
    ) -> anyhow::Result<Self> {
        let mut next = self.replace(range.clone(), text)?;
        if range.end - range.start == text.chars().count() {
            for (run, id) in self.snapshot.properties().runs() {
                let start = run.start.0.max(range.start);
                let end = run.end.0.min(range.end);
                if start < end {
                    next.snapshot =
                        next.snapshot.set_properties(CharPos(start)..CharPos(end), id)?;
                }
            }
            // In-place-width substitution preserves the CURRENT live marker
            // registry, unlike a deletion+insertion coordinate transformation.
            next.snapshot = self.snapshot.restore_content(&next.snapshot, None)?;
            if let Some(binding) = &self.binding {
                next.binding = Some(Binding {
                    lease: binding.lease.retain_for_snapshot(
                        next.snapshot.properties().runs().map(|(_, id)| id.0),
                        &binding.insertion,
                    )?,
                    insertion: binding.insertion.clone(),
                });
            }
        }
        Ok(next)
    }
    fn line_start_offset(&self, at:usize, delta:i64)->Option<usize> {
        let text=self.snapshot.text();let line=text.char_to_line(CharPos(at)).ok()?.0;
        let target=if delta>=0 {line.saturating_add(usize::try_from(delta).unwrap_or(usize::MAX)).min(text.line_count()-1)}else{line.saturating_sub(usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX))};
        text.line_to_char(rune_text::LinePos(target)).ok().map(|p|p.0)
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
    disk_baselines: std::collections::BTreeMap<rune_editor::BufferId, save::guarded::Baseline>,
    read_only_values: std::collections::BTreeMap<rune_editor::BufferId, Arc<crate::core::object::HeapDomainValue>>,
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
        Ok(Self { session, properties, disk_baselines: Default::default(), read_only_values:Default::default() })
    }
    fn prune_disk_baselines(&mut self) {
        self.disk_baselines.retain(|id,_| self.session.buffer_live(*id));
        self.read_only_values.retain(|id,_| self.session.buffer_snapshot(*id).is_ok_and(|b|b.read_only));
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
impl RuntimeEditorHost<'_, '_> {
    fn session(&self) -> &rune_editor::Session<RuntimeDocument> {
        self.editor.session()
    }
}
impl rune_editor::EditorHost for RuntimeEditorHost<'_, '_> {
    type Doc = RuntimeDocument;
    fn snapshot(&self) -> rune_editor::Result<rune_editor::SessionSnapshot<RuntimeDocument>> {
        Ok(self.editor.session.snapshot())
    }
    fn execute(
        &mut self,
        c: rune_editor::Command,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        let result = self.editor.session.execute(c);
        self.editor.prune_disk_baselines();
        result
    }
    fn dispatch_key(&mut self, k: rune_editor::Key) -> rune_editor::Result<rune_editor::Dispatch> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        let result = self.editor.session.dispatch(k);
        self.editor.prune_disk_baselines();
        result
    }
    fn cancel_prefix(&mut self) -> rune_editor::Result<()> {
        self.editor
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        self.editor.session.cancel_prefix();
        Ok(())
    }
    fn scroll_window(&mut self,w:rune_editor::WindowId,b:rune_editor::BufferId,sequence:u64,lines:i64)->rune_editor::Result<bool> {
        self.editor.properties.empty(self.cx).map_err(|e|rune_editor::EditorError::Backend(e.to_string()))?; self.editor.session.scroll_window(w,b,sequence,lines)
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
#[cfg(test)]
mod gateway_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn owner_metadata_preparation_rejects_stale_publish_after_native_edit() {
        let roots = RootSet::default();
        let mut cx = Context::new(&roots);
        let mut editor = RuntimeEditor::new(&cx, 80, 24).unwrap();
        let b = editor.create_buffer("target", "λ", &cx).unwrap();
        // SAFETY: fresh scalar payload belongs to this Context.
        let property = unsafe { editor.intern_property_set(cx.add("face"), &cx) }.unwrap();
        let prepared =
            editor.prepare_property_set(b, CharPos(0)..CharPos(1), &property, &cx).unwrap();
        {
            let mut host = RuntimeEditorHost { editor: &mut editor, cx: &mut cx };
            host.execute(Command::SwitchBuffer(b)).unwrap();
            host.execute(Command::Insert("x".into())).unwrap();
        }
        assert!(editor.publish_property_set(prepared, &cx).is_err());
        assert_eq!(editor.session().document(b).unwrap().to_text(), "xλ");
        assert!(editor.session().document(b).unwrap().resolve(property.id(), &cx).is_err());
    }
}

/// Stable identifier for a future LispBuffer shell. No Context, GC pointer,
/// mutable text or owner borrow crosses the shell boundary. Construction stays
/// private to the authoritative runtime owner; numeric IDs are not adoption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OwnerBufferHandle {
    owner: u64,
    buffer: rune_editor::BufferId,
}
impl OwnerBufferHandle {
    /// Immutable owner stamp only. No editor borrow, liveness or membership proof.
    /// Attachment construction still requires the authenticated pair transaction
    /// and separate Context/Buffer liveness witnesses.
    pub(crate) fn belongs_to(&self, cx: &Context) -> bool {
        self.owner == cx.domain_id()
    }
}
impl RuntimeEditor {
    pub(crate) fn buffer_handle(
        &self,
        buffer: rune_editor::BufferId,
        cx: &Context,
    ) -> anyhow::Result<OwnerBufferHandle> {
        self.properties.empty(cx)?;
        anyhow::ensure!(self.session.buffer_live(buffer), "Deleted editor buffer");
        Ok(OwnerBufferHandle { owner: cx.domain_id(), buffer })
    }
    fn validate_handle(
        &self,
        handle: OwnerBufferHandle,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::BufferId> {
        self.properties.empty(cx)?;
        anyhow::ensure!(handle.owner == cx.domain_id(), "Foreign editor owner");
        anyhow::ensure!(self.session.buffer_live(handle.buffer), "Deleted editor buffer");
        Ok(handle.buffer)
    }
    pub(crate) fn snapshot_handle(
        &self,
        handle: OwnerBufferHandle,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::BufferSnapshot<RuntimeDocument>> {
        Ok(self.session.buffer_snapshot(self.validate_handle(handle, cx)?)?)
    }
    pub(crate) fn select_lisp_handle(
        &mut self,
        handle: OwnerBufferHandle,
        cx: &Context,
    ) -> anyhow::Result<()> {
        let id = self.validate_handle(handle, cx)?;
        Ok(self.session.set_current_buffer(id)?)
    }
}
#[cfg(test)]
mod storage_boundary_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn stable_shell_selects_owner_storage_without_changing_selected_window() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let mut owner = RuntimeEditor::new(&cx, 80, 24).unwrap();
        let selected = owner.session().selected_window();
        let original = owner.session().snapshot().views[0].view.buffer;
        let background = owner.create_buffer("background", "λ🙂", &cx).unwrap();
        let handle = owner.buffer_handle(background, &cx).unwrap();
        owner.select_lisp_handle(handle, &cx).unwrap();
        owner.replace_current_lisp_range(2, 3, "界", &cx).unwrap();
        assert_eq!(owner.snapshot_handle(handle, &cx).unwrap().document.to_text(), "λ界");
        assert_eq!(owner.session().window_tree().view(selected).unwrap().buffer, original);
        std::thread::spawn(move || {
            let foreign_roots = RootSet::default();
            let foreign = Context::new(&foreign_roots);
            let other = RuntimeEditor::new(&foreign, 80, 24).unwrap();
            assert!(other.snapshot_handle(handle, &foreign).is_err());
        })
        .join()
        .unwrap();
    }
}

/// Immutable owned read. No owner/Env/RefCell borrow survives a method return.
#[derive(Clone, Debug)]
pub(crate) struct BufferRead {
    pub(crate) handle: OwnerBufferHandle,
    pub(crate) epoch: u64,
    pub(crate) document: RuntimeDocument,
    pub(crate) point: usize,
    pub(crate) name: String,
    pub(crate) file: rune_editor::FileAssociation,
}
/// Explicit owner-local gateway for Env. Never put this Rc in global LispBuffer.
/// No callback/Lisp evaluation occurs while a RefCell borrow is held.
#[derive(Clone)]
pub(crate) struct RuntimeEditorGateway {
    owner: u64,
    inner: std::rc::Rc<std::cell::RefCell<RuntimeEditor>>,
    abandoned_markers:
        std::rc::Rc<std::cell::RefCell<Vec<(OwnerBufferHandle, rune_editor::MarkerId)>>>,
}
impl RuntimeEditorGateway {
    pub(crate) fn new(cx: &Context, width: u32, height: u32) -> anyhow::Result<Self> {
        Ok(Self {
            owner: cx.domain_id(),
            abandoned_markers: Default::default(),
            inner: std::rc::Rc::new(std::cell::RefCell::new(RuntimeEditor::new(
                cx, width, height,
            )?)),
        })
    }
    fn check(&self, cx: &Context) -> anyhow::Result<()> {
        anyhow::ensure!(self.owner == cx.domain_id(), "Foreign editor gateway");
        if !self.abandoned_markers.borrow().is_empty() {
            // Acquire owner before taking queue, so busy-owner errors retain all
            // cleanup records for the next entry rather than losing them.
            let mut owner = self.inner.try_borrow_mut()?;
            let abandoned = std::mem::take(&mut *self.abandoned_markers.borrow_mut());
            for (handle, marker) in abandoned {
                if owner.validate_handle(handle, cx).is_ok() {
                    owner.session.remove_marker(handle.buffer, marker)?;
                }
            }
        }
        Ok(())
    }
    /// Geometry of the same authoritative window tree, without cloning docs.
    pub(crate) fn frame_geometry(&self,cx:&Context)->anyhow::Result<(u32,u32)> {
        self.check(cx)?;let owner=self.inner.try_borrow()?;let tree=owner.session.window_tree();
        let rect=tree.node(tree.root()).ok_or_else(||anyhow::anyhow!("Missing root window geometry"))?.rect;
        Ok((rect.width,rect.height))
    }
    pub(crate) fn scratch(&self, cx: &Context) -> anyhow::Result<OwnerBufferHandle> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        owner.buffer_handle(owner.session.current_buffer(), cx)
    }
    pub(crate) fn create(
        &self,
        name: &str,
        text: &str,
        cx: &Context,
    ) -> anyhow::Result<OwnerBufferHandle> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.create_buffer(name, text, cx)?;
        owner.buffer_handle(id, cx)
    }
    pub(crate) fn read(
        &self,
        handle: OwnerBufferHandle,
        cx: &Context,
    ) -> anyhow::Result<BufferRead> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        let id = owner.validate_handle(handle, cx)?;
        let root = owner.session.buffer_snapshot(id)?;
        Ok(BufferRead {
            handle,
            epoch: root.version,
            document: root.document,
            point: owner.session.buffer_point(id)?,
            name: root.name,
            file: root.file,
        })
    }
    pub(crate) fn set_point(
        &self,
        handle: OwnerBufferHandle,
        point: usize,
        cx: &Context,
    ) -> anyhow::Result<()> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.set_buffer_point(id, point)?)
    }
    pub(crate) fn splice(
        &self,
        handle: OwnerBufferHandle,
        epoch: u64,
        range: Range<usize>,
        text: &str,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::CommandOutcome> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.replace_buffer(id, epoch, range, text)?)
    }
    pub(crate) fn register_marker(
        &self,
        handle: OwnerBufferHandle,
        point: usize,
        bias: Bias,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::MarkerId> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.register_marker(id, point, bias)?)
    }
    pub(crate) fn marker_position(
        &self,
        handle: OwnerBufferHandle,
        marker: rune_editor::MarkerId,
        cx: &Context,
    ) -> anyhow::Result<Option<usize>> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.marker_position(id, marker)?)
    }
    pub(crate) fn remove_marker(
        &self,
        handle: OwnerBufferHandle,
        marker: rune_editor::MarkerId,
        cx: &Context,
    ) -> anyhow::Result<()> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.remove_marker(id, marker)?)
    }
}
#[cfg(test)]
mod gateway_storage_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn cloned_gateway_has_one_storage_and_owned_reads_do_not_hold_borrows() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let gateway = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = gateway.create("background", "λ🙂", &cx).unwrap();
        let old = gateway.read(h, &cx).unwrap();
        let clone = gateway.clone();
        clone.splice(h, old.epoch, 1..2, "界", &cx).unwrap();
        assert_eq!(old.document.to_text(), "λ🙂");
        assert_eq!(gateway.read(h, &cx).unwrap().document.to_text(), "λ界");
        assert!(gateway.splice(h, old.epoch, 0..0, "stale", &cx).is_err());
        let marker = clone.register_marker(h, 1, Bias::Right, &cx).unwrap();
        assert_eq!(gateway.marker_position(h, marker, &cx).unwrap(), Some(1));
        gateway.remove_marker(h, marker, &cx).unwrap();
        assert_eq!(clone.marker_position(h, marker, &cx).unwrap(), None);
    }
}
#[cfg(test)]
mod gateway_lifecycle_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn retired_owner_gateway_rejects_rebinding_and_old_snapshot_ids() {
        let roots = RootSet::default();
        let (gateway, handle, old) = {
            let cx = Context::new(&roots);
            let gateway = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
            let h = gateway.create("retained", "λ", &cx).unwrap();
            let old = gateway.read(h, &cx).unwrap();
            (gateway, h, old)
        };
        let next_roots = RootSet::default();
        let next = Context::new(&next_roots);
        assert!(gateway.read(handle, &next).is_err());
        assert!(gateway.set_point(handle, 0, &next).is_err());
        assert!(gateway.splice(handle, old.epoch, 0..0, "bad", &next).is_err());
        // Immutable Unicode content remains readable; no Lisp resolution is
        // implied by retaining the native snapshot beyond Context teardown.
        assert_eq!(old.document.to_text(), "λ");
        assert!(old.document.resolve(0, &next).is_err());
    }
    #[test]
    fn distinct_gateways_cannot_adopt_each_others_handles_in_same_context() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let first = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let second = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = first.create("private", "x", &cx).unwrap();
        assert!(second.read(h, &cx).is_err());
        let old = first.read(h, &cx).unwrap();
        second.create("private", "other", &cx).unwrap();
        assert!(second.splice(h, old.epoch, 0..1, "wrong", &cx).is_err());
        assert_eq!(first.read(h, &cx).unwrap().document.to_text(), "x");
    }
}

/// Native bridge over the SAME owner-local gateway Env uses. Each query returns
/// owned roots; no RefCell guard or owner borrow crosses the API boundary.
pub(crate) struct GatewayEditorHost<'a, 'roots> {
    gateway: RuntimeEditorGateway,
    cx: &'a Context<'roots>,
}
impl RuntimeEditorGateway {
    pub(crate) fn host<'a, 'roots>(
        &self,
        cx: &'a Context<'roots>,
    ) -> anyhow::Result<GatewayEditorHost<'a, 'roots>> {
        self.check(cx)?;
        Ok(GatewayEditorHost { gateway: self.clone(), cx })
    }
}
impl GatewayEditorHost<'_, '_> {
    fn mutate<R>(
        &mut self,
        run: impl FnOnce(&mut RuntimeEditor) -> rune_editor::Result<R>,
    ) -> rune_editor::Result<R> {
        self.gateway
            .check(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        let mut owner = self
            .gateway
            .inner
            .try_borrow_mut()
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        owner
            .properties
            .empty(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        let result = run(&mut owner);
        owner.prune_disk_baselines();
        result
    }
}
impl rune_editor::EditorHost for GatewayEditorHost<'_, '_> {
    type Doc = RuntimeDocument;
    fn snapshot(&self) -> rune_editor::Result<rune_editor::SessionSnapshot<RuntimeDocument>> {
        self.gateway
            .check(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        let owner = self
            .gateway
            .inner
            .try_borrow()
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?;
        Ok(owner.session.snapshot())
    }
    fn execute(
        &mut self,
        c: rune_editor::Command,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.mutate(|o| o.session.execute(c))
    }
    fn dispatch_key(&mut self, k: rune_editor::Key) -> rune_editor::Result<rune_editor::Dispatch> {
        self.mutate(|o| o.session.dispatch(k))
    }
    fn cancel_prefix(&mut self) -> rune_editor::Result<()> {
        self.mutate(|owner| {
            owner.session.cancel_prefix();
            Ok(())
        })
    }
    fn scroll_window(&mut self,w:rune_editor::WindowId,b:rune_editor::BufferId,sequence:u64,lines:i64)->rune_editor::Result<bool> {
        self.mutate(|owner|owner.session.scroll_window(w,b,sequence,lines))
    }
    fn place_pointer(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        p: usize,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.mutate(|o| o.session.place_pointer(w, b, e, p))
    }
    fn commit_replacement(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        r: Range<usize>,
        t: String,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        self.mutate(|o| o.session.commit_replacement(w, b, e, r, t))
    }
}
#[cfg(test)]
mod shared_native_gateway_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn native_and_lisp_gateway_share_authority_without_long_query_borrow() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let gateway = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let scratch = gateway.scratch(&cx).unwrap();
        let mut native = gateway.host(&cx).unwrap();
        let frame = native.snapshot().unwrap();
        let read = gateway.read(scratch, &cx).unwrap();
        gateway.splice(scratch, read.epoch, 0..0, "λ🙂", &cx).unwrap();
        assert_eq!(frame.buffers[0].document.to_text(), "");
        native.execute(Command::Undo).unwrap();
        assert_eq!(gateway.read(scratch, &cx).unwrap().document.to_text(), "");
        native.execute(Command::Redo).unwrap();
        assert_eq!(gateway.read(scratch, &cx).unwrap().document.to_text(), "λ🙂");
        let guard = gateway.inner.borrow_mut();
        assert!(native.snapshot().is_err());
        drop(guard);
        assert!(native.snapshot().is_ok());
    }
}
impl rune_editor::PlatformTextDocument for RuntimeDocument {
    fn char_to_utf16(&self, p: usize) -> rune_editor::Result<usize> {
        self.snapshot
            .text()
            .char_to_utf16(CharPos(p))
            .map(|p| p.0)
            .map_err(|_| rune_editor::EditorError::InvalidPosition)
    }
    fn utf16_to_char(&self, p: usize) -> rune_editor::Result<usize> {
        self.snapshot
            .text()
            .utf16_to_char(rune_text::Utf16Pos(p))
            .map(|p| p.0)
            .map_err(|_| rune_editor::EditorError::InvalidPosition)
    }
}
#[cfg(test)]
mod platform_metric_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::PlatformTextDocument;
    #[test]
    fn runtime_document_utf16_conversion_uses_scalar_boundaries() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let registry = PropertyRegistry::new(&cx);
        let d = RuntimeDocument::new("a🙂λ", &registry, &cx).unwrap();
        assert_eq!(d.char_to_utf16(2).unwrap(), 3);
        assert_eq!(d.utf16_to_char(3).unwrap(), 2);
        assert!(d.utf16_to_char(2).is_err());
        assert!(d.char_to_utf16(4).is_err());
    }
}
impl RuntimeEditorGateway {
    pub(crate) fn lookup(
        &self,
        name: &str,
        cx: &Context,
    ) -> anyhow::Result<Option<OwnerBufferHandle>> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        owner.session.get_buffer(name).map(|id| owner.buffer_handle(id, cx)).transpose()
    }
    pub(crate) fn list(&self, cx: &Context) -> anyhow::Result<Vec<OwnerBufferHandle>> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        owner
            .session
            .buffer_list()
            .iter()
            .map(|id| owner.buffer_handle(*id, cx))
            .collect()
    }
    pub(crate) fn rename(
        &self,
        handle: OwnerBufferHandle,
        name: &str,
        unique: bool,
        cx: &Context,
    ) -> anyhow::Result<String> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        owner.session.execute(rune_editor::Command::RenameBuffer {
            buffer: id,
            name: name.into(),
            unique,
        })?;
        Ok(owner
            .session
            .buffer_name(id)
            .ok_or_else(|| anyhow::anyhow!("Deleted editor buffer"))?
            .into())
    }
    /// Return the surviving fallback handle after kill; Env selects it only if
    /// its own current binding was killed. Other Env bindings stay unchanged.
    pub(crate) fn kill(
        &self,
        handle: OwnerBufferHandle,
        force: bool,
        cx: &Context,
    ) -> anyhow::Result<OwnerBufferHandle> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        owner.session.execute(rune_editor::Command::KillBuffer { buffer: id, force })?;
        owner.disk_baselines.remove(&id);
        owner.read_only_values.remove(&id);
        owner.buffer_handle(owner.session.current_buffer(), cx)
    }
}
#[cfg(test)]
mod gateway_registry_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn owner_lookup_rename_and_kill_preserve_handle_identity_and_reject_stale_shell() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("owned", "λ", &cx).unwrap();
        assert_eq!(g.lookup("owned", &cx).unwrap(), Some(h));
        assert!(g.list(&cx).unwrap().contains(&h));
        assert_eq!(g.rename(h, "renamed", false, &cx).unwrap(), "renamed");
        assert_eq!(g.lookup("owned", &cx).unwrap(), None);
        assert_eq!(g.lookup("renamed", &cx).unwrap(), Some(h));
        let fallback = g.kill(h, true, &cx).unwrap();
        assert_ne!(fallback, h);
        assert!(g.read(h, &cx).is_err());
        assert!(g.read(fallback, &cx).is_ok());
    }
}
impl RuntimeEditorGateway {
    /// All reachable local payload values must belong to cx/audited global
    /// storage and remain rooted. Destination checks do not prove provenance.
    pub(crate) unsafe fn intern_property(
        &self,
        value: Object,
        cx: &Context,
    ) -> anyhow::Result<PropertySetHandle> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        // SAFETY: forwarded caller's reachable-graph provenance/rooting contract.
        unsafe { owner.intern_property_set(value, cx) }
    }
    /// Prepare from the exact immutable read used for owner-side plist merging.
    /// No Lisp callbacks run under this short allocator borrow.
    pub(crate) fn prepare_properties(
        &self,
        read: &BufferRead,
        range: Range<CharPos>,
        handle: &PropertySetHandle,
        cx: &Context,
    ) -> anyhow::Result<PreparedMetadata> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        let buffer = owner.validate_handle(read.handle, cx)?;
        let document = read.document.set_properties(range, handle, &owner.properties, cx)?;
        Ok(PreparedMetadata { buffer, epoch: read.epoch, document })
    }
    pub(crate) fn publish_properties(
        &self,
        prepared: PreparedMetadata,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::CommandOutcome> {
        self.check(cx)?;
        self.inner.try_borrow_mut()?.publish_property_set(prepared, cx)
    }
}
#[cfg(test)]
mod property_gateway_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn captured_property_preparation_preserves_live_markers_and_rejects_old_epoch() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("styled", "λ🙂", &cx).unwrap();
        let removed = g.register_marker(h, 1, Bias::Left, &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        // SAFETY: scalar string is allocated in exactly this Context.
        let style = unsafe { g.intern_property(cx.add("face"), &cx) }.unwrap();
        let prepared = g.prepare_properties(&read, CharPos(0)..CharPos(2), &style, &cx).unwrap();
        g.remove_marker(h, removed, &cx).unwrap();
        let fresh = g.register_marker(h, 2, Bias::Right, &cx).unwrap();
        g.publish_properties(prepared, &cx).unwrap();
        assert_eq!(g.marker_position(h, removed, &cx).unwrap(), None);
        assert_eq!(g.marker_position(h, fresh, &cx).unwrap(), Some(2));
        let stale = g.prepare_properties(&read, CharPos(0)..CharPos(1), &style, &cx).unwrap();
        assert!(g.publish_properties(stale, &cx).is_err());
        let mut native = g.host(&cx).unwrap();
        native.execute(Command::SwitchBuffer(h.buffer)).unwrap();
        native.execute(Command::Undo).unwrap();
        assert_eq!(g.marker_position(h, removed, &cx).unwrap(), None);
        assert_eq!(g.marker_position(h, fresh, &cx).unwrap(), Some(2));
        native.execute(Command::Redo).unwrap();
        assert_eq!(g.marker_position(h, removed, &cx).unwrap(), None);
        assert_eq!(g.read(h, &cx).unwrap().document.resolve(style.id(), &cx).unwrap(), "face");
    }
}
impl RuntimeEditorGateway {
    /// Ordered updates use last-write-wins for overlaps. Preparation is local:
    /// any invalid range/foreign handle rejects the whole prepared root. Caller
    /// compares Lisp values and skips no-effective-change before calling this.
    pub(crate) fn prepare_property_runs(
        &self,
        read: &BufferRead,
        updates: impl IntoIterator<Item = (Range<CharPos>, PropertySetHandle)>,
        cx: &Context,
    ) -> anyhow::Result<PreparedMetadata> {
        self.check(cx)?;
        // User-supplied iterator code runs before any owner borrow.
        let updates: Vec<_> = updates.into_iter().collect();
        let owner = self.inner.try_borrow()?;
        let buffer = owner.validate_handle(read.handle, cx)?;
        let mut document = read.document.clone();
        for (range, handle) in updates {
            document = document.set_properties(range, &handle, &owner.properties, cx)?;
        }
        Ok(PreparedMetadata { buffer, epoch: read.epoch, document })
    }
}
#[cfg(test)]
mod property_batch_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn property_run_batch_publishes_once_and_failure_leaves_owner_unchanged() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("batch", "abc", &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        // SAFETY: freshly allocated scalar strings in the same Context.
        let a = unsafe { g.intern_property(cx.add("a"), &cx) }.unwrap();
        let b = unsafe { g.intern_property(cx.add("b"), &cx) }.unwrap();
        assert!(
            g.prepare_property_runs(
                &read,
                [(CharPos(0)..CharPos(1), a.clone()), (CharPos(2)..CharPos(4), b.clone())],
                &cx
            )
            .is_err()
        );
        assert_eq!(g.read(h, &cx).unwrap().epoch, read.epoch);
        let prepared = g
            .prepare_property_runs(
                &read,
                [(CharPos(0)..CharPos(3), a.clone()), (CharPos(1)..CharPos(2), b.clone())],
                &cx,
            )
            .unwrap();
        g.publish_properties(prepared, &cx).unwrap();
        let now = g.read(h, &cx).unwrap();
        assert_eq!(now.epoch, read.epoch + 1);
        assert_eq!(now.document.properties().at(CharPos(0)), Some(PropSetId(a.id())));
        assert_eq!(now.document.properties().at(CharPos(1)), Some(PropSetId(b.id())));
        assert_eq!(now.document.properties().at(CharPos(2)), Some(PropSetId(a.id())));
    }
}
/// Production owner-thread factory: obtains the Context's sole gateway, exactly
/// the same store Env::new(cx) uses. No second RuntimeEditor is constructed.
/// It still installs no legacy Lisp primitive bindings or native platform loop.
pub(crate) fn with_shared_runtime_owner<R>(
    width: u32,
    height: u32,
    run: impl for<'a, 'roots> FnOnce(GatewayEditorHost<'a, 'roots>) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    use rune_editor::EditorHost;
    let roots = crate::core::gc::RootSet::default();
    let cx = Context::new(&roots);
    let gateway = cx.editor_gateway()?;
    let mut host = gateway.host(&cx)?;
    host.execute(rune_editor::Command::Resize { width, height })?;
    run(host)
}
#[cfg(test)]
mod shared_factory_tests {
    use super::*;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn production_factory_native_and_context_accessor_have_identical_storage() {
        with_shared_runtime_owner(90, 30, |mut host| {
            let env_gateway = host.cx.editor_gateway()?;
            let scratch = env_gateway.scratch(host.cx)?;
            let read = env_gateway.read(scratch, host.cx)?;
            env_gateway.splice(scratch, read.epoch, 0..0, "λ🙂", host.cx)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "λ🙂");
            host.execute(rune_editor::Command::Undo)?;
            assert_eq!(env_gateway.read(scratch, host.cx)?.document.to_text(), "");
            Ok(())
        })
        .unwrap();
    }
}
#[cfg(test)]
mod session_identity_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn same_context_foreign_session_rejects_all_shell_operations() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let first = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let second = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let foreign = first.scratch(&cx).unwrap();
        let local = second.scratch(&cx).unwrap();
        let epoch = second.read(local, &cx).unwrap().epoch;
        for handle in [foreign] {
            assert!(second.read(handle, &cx).is_err());
            assert!(second.rename(handle, "wrong", false, &cx).is_err());
            assert!(second.kill(handle, true, &cx).is_err());
            assert!(second.splice(handle, epoch, 0..0, "wrong", &cx).is_err());
        }
        assert_eq!(second.read(local, &cx).unwrap().document.to_text(), "");
        assert_eq!(second.read(local, &cx).unwrap().name, "*scratch*");
    }
}

/// Mutable Context owner for future evaluator dispatch. The native trait still
/// receives owned snapshots; shared edit hosts are constructed only per call.
/// This avoids an immutable Context borrow spanning the worker's entire life.
pub(crate) struct MutableGatewayEditorHost<'a, 'roots> {
    gateway: RuntimeEditorGateway,
    cx: &'a mut Context<'roots>,
}
impl MutableGatewayEditorHost<'_, '_> {
    /// Evaluator work runs with NO editor RefCell borrow held. Env must obtain
    /// this Context's gateway, never construct another authoritative Session.
    /// This is an ownership boundary, NOT a whole-evaluation transaction: Lisp
    /// may edit before signalling an error. Never retry the callback to recover
    /// later failures; each gateway mutation already owns its undo transaction.
    pub(crate) fn with_context<R>(
        &mut self,
        run: impl FnOnce(&mut Context, &RuntimeEditorGateway) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        self.gateway.check(self.cx)?;
        run(self.cx, &self.gateway)
    }
}
impl rune_editor::EditorHost for MutableGatewayEditorHost<'_, '_> {
    type Doc = RuntimeDocument;
    fn snapshot(&self) -> rune_editor::Result<rune_editor::SessionSnapshot<RuntimeDocument>> {
        use rune_editor::EditorHost;
        self.gateway
            .host(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?
            .snapshot()
    }
    fn execute(
        &mut self,
        c: rune_editor::Command,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        use rune_editor::EditorHost;
        self.gateway
            .host(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?
            .execute(c)
    }
    fn dispatch_key(&mut self, k: rune_editor::Key) -> rune_editor::Result<rune_editor::Dispatch> {
        use rune_editor::EditorHost;
        self.gateway
            .host(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?
            .dispatch_key(k)
    }
    fn cancel_prefix(&mut self) -> rune_editor::Result<()> {
        use rune_editor::EditorHost;
        self.gateway
            .host(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?
            .cancel_prefix()
    }
    fn scroll_window(&mut self,w:rune_editor::WindowId,b:rune_editor::BufferId,sequence:u64,lines:i64)->rune_editor::Result<bool> {
        use rune_editor::EditorHost; self.gateway.host(self.cx).map_err(|e|rune_editor::EditorError::Backend(e.to_string()))?.scroll_window(w,b,sequence,lines)
    }
    fn place_pointer(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        p: usize,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        use rune_editor::EditorHost;
        self.gateway
            .host(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?
            .place_pointer(w, b, e, p)
    }
    fn commit_replacement(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        r: Range<usize>,
        t: String,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        use rune_editor::EditorHost;
        self.gateway
            .host(self.cx)
            .map_err(|e| rune_editor::EditorError::Backend(e.to_string()))?
            .commit_replacement(w, b, e, r, t)
    }
}
pub(crate) fn with_evaluator_runtime_owner<R>(
    width: u32,
    height: u32,
    run: impl for<'a, 'roots> FnOnce(MutableGatewayEditorHost<'a, 'roots>) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    use rune_editor::EditorHost;
    let roots = crate::core::gc::RootSet::default();
    let mut cx = Context::new(&roots);
    let gateway = cx.editor_gateway()?;
    let mut host = MutableGatewayEditorHost { gateway, cx: &mut cx };
    host.execute(rune_editor::Command::Resize { width, height })?;
    run(host)
}
#[cfg(test)]
mod mutable_owner_tests {
    use super::*;
    use rune_editor::{Command, Document, EditorHost};
    #[test]
    fn evaluator_context_gc_and_gateway_edits_run_without_retained_owner_borrow() {
        with_evaluator_runtime_owner(80, 24, |mut host| {
            let old = host.snapshot()?;
            host.with_context(|cx, gateway| {
                let h = gateway.scratch(cx)?;
                let read = gateway.read(h, cx)?;
                gateway.splice(h, read.epoch, 0..0, "λ🙂", cx)?;
                cx.garbage_collect(true);
                Ok(())
            })?;
            assert_eq!(old.buffers[0].document.to_text(), "");
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "λ🙂");
            host.execute(Command::Undo)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "");
            Ok(())
        })
        .unwrap();
    }
}
#[cfg(test)]
mod evaluator_partial_effect_tests {
    use super::*;
    use rune_editor::{Command, Document, EditorHost};
    #[test]
    fn evaluator_error_does_not_claim_rollback_or_replay_applied_edit() {
        with_evaluator_runtime_owner(80, 24, |mut host| {
            let outcome = host.with_context(|cx, gateway| -> anyhow::Result<()> {
                let h = gateway.scratch(cx)?;
                let read = gateway.read(h, cx)?;
                gateway.splice(h, read.epoch, 0..0, "once", cx)?;
                anyhow::bail!("Lisp signal after edit")
            });
            assert!(outcome.is_err());
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "once");
            host.execute(Command::Undo)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "");
            Ok(())
        })
        .unwrap();
    }
}

/// Native-only unwind cleanup. Drop never resolves Lisp values, borrows Session,
/// restores point, or claims rollback; it merely queues exact unregister intent.
pub(crate) struct MarkerCleanupGuard {
    record: Option<(OwnerBufferHandle, rune_editor::MarkerId)>,
    pending: std::rc::Rc<std::cell::RefCell<Vec<(OwnerBufferHandle, rune_editor::MarkerId)>>>,
}
impl MarkerCleanupGuard {
    pub(crate) fn disarm(mut self) {
        self.record = None;
    }
}
impl Drop for MarkerCleanupGuard {
    fn drop(&mut self) {
        if let Some(record) = self.record.take() {
            self.pending.borrow_mut().push(record);
        }
    }
}
impl RuntimeEditorGateway {
    pub(crate) fn register_temporary_marker(
        &self,
        handle: OwnerBufferHandle,
        point: usize,
        bias: Bias,
        cx: &Context,
    ) -> anyhow::Result<(rune_editor::MarkerId, MarkerCleanupGuard)> {
        let marker = self.register_marker(handle, point, bias, cx)?;
        Ok((
            marker,
            MarkerCleanupGuard {
                record: Some((handle, marker)),
                pending: self.abandoned_markers.clone(),
            },
        ))
    }
}
#[cfg(test)]
mod marker_unwind_cleanup_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn abandoned_marker_cleanup_never_locks_owner_during_unwind_and_preserves_unrelated_ids() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.scratch(&cx).unwrap();
        let unrelated = g.register_marker(h, 0, Bias::Right, &cx).unwrap();
        let (temporary, guard) = g.register_temporary_marker(h, 0, Bias::Left, &cx).unwrap();
        let owner_guard = g.inner.borrow_mut();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _cleanup = guard;
            panic!("test evaluator unwind");
        }));
        assert!(panic.is_err());
        assert_eq!(g.abandoned_markers.borrow().len(), 1);
        assert!(g.read(h, &cx).is_err());
        assert_eq!(g.abandoned_markers.borrow().len(), 1);
        drop(owner_guard);
        assert_eq!(g.marker_position(h, temporary, &cx).unwrap(), None);
        assert_eq!(g.marker_position(h, unrelated, &cx).unwrap(), Some(0));
        assert!(g.abandoned_markers.borrow().is_empty());
        let (normal, guard) = g.register_temporary_marker(h, 0, Bias::Left, &cx).unwrap();
        g.remove_marker(h, normal, &cx).unwrap();
        guard.disarm();
        assert!(g.abandoned_markers.borrow().is_empty());
    }
}
#[cfg(test)]
mod cleanup_retirement_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn stable_shell_is_send_sync_but_cleanup_is_owner_local_and_skips_killed_buffer() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<OwnerBufferHandle>();
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("temporary", "λ", &cx).unwrap();
        let (_, guard) = g.register_temporary_marker(h, 1, Bias::Right, &cx).unwrap();
        let fallback = g.kill(h, true, &cx).unwrap();
        drop(guard);
        assert_eq!(g.abandoned_markers.borrow().len(), 1);
        assert!(g.read(fallback, &cx).is_ok());
        assert!(g.abandoned_markers.borrow().is_empty());
        assert!(g.read(h, &cx).is_err());
    }
    #[test]
    fn cleanup_drop_after_context_retirement_never_resolves_lisp_or_adopts_new_owner() {
        let roots = RootSet::default();
        let (g, guard) = {
            let cx = Context::new(&roots);
            let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
            let h = g.scratch(&cx).unwrap();
            let (_, guard) = g.register_temporary_marker(h, 0, Bias::Left, &cx).unwrap();
            (g, guard)
        };
        drop(guard);
        assert_eq!(g.abandoned_markers.borrow().len(), 1);
        let next_roots = RootSet::default();
        let next = Context::new(&next_roots);
        assert!(g.scratch(&next).is_err());
        drop(g); // Native queue/session teardown only; no old Context exists.
    }
}
impl std::fmt::Debug for RuntimeEditorGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never borrow the live editor for debugging (including during unwind).
        f.debug_struct("RuntimeEditorGateway").finish_non_exhaustive()
    }
}
impl std::fmt::Debug for MarkerCleanupGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkerCleanupGuard")
            .field("armed", &self.record.is_some())
            .finish_non_exhaustive()
    }
}
#[cfg(test)]
mod gateway_property_identity_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn late_foreign_property_handle_rejects_entire_batch_even_equal_numeric_ids() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let first = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let second = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = first.create("owned", "ab", &cx).unwrap();
        let read = first.read(h, &cx).unwrap();
        // SAFETY: both scalar payloads belong to this Context; their distinct
        // registry provenance must still reject mixing equal numeric IDs.
        let local = unsafe { first.intern_property(cx.add("local"), &cx) }.unwrap();
        let foreign = unsafe { second.intern_property(cx.add("foreign"), &cx) }.unwrap();
        assert_eq!(local.id(), foreign.id());
        assert!(
            first
                .prepare_property_runs(
                    &read,
                    [(CharPos(0)..CharPos(1), local), (CharPos(1)..CharPos(2), foreign)],
                    &cx
                )
                .is_err()
        );
        let unchanged = first.read(h, &cx).unwrap();
        assert_eq!(unchanged.epoch, read.epoch);
        assert_eq!(unchanged.document.properties().at(CharPos(0)), Some(PropSetId(0)));
        assert_eq!(unchanged.document.properties().at(CharPos(1)), Some(PropSetId(0)));
    }
}
impl RuntimeEditorGateway {
    pub(crate) fn replace_preserving_properties(
        &self,
        handle: OwnerBufferHandle,
        epoch: u64,
        range: Range<usize>,
        text: &str,
        cx: &Context,
    ) -> anyhow::Result<rune_editor::CommandOutcome> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.replace_buffer_preserving_properties(id, epoch, range, text)?)
    }
}
#[cfg(test)]
mod case_property_transaction_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn same_width_case_replacement_keeps_each_property_and_is_one_undo() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("case", "aβ tail", &cx).unwrap();
        let before = g.read(h, &cx).unwrap();
        // SAFETY: fresh scalar values in this Context.
        let a = unsafe { g.intern_property(cx.add("first"), &cx) }.unwrap();
        let b = unsafe { g.intern_property(cx.add("second"), &cx) }.unwrap();
        let prepared = g
            .prepare_property_runs(
                &before,
                [(CharPos(0)..CharPos(1), a.clone()), (CharPos(1)..CharPos(2), b.clone())],
                &cx,
            )
            .unwrap();
        g.publish_properties(prepared, &cx).unwrap();
        let before = g.read(h, &cx).unwrap();
        g.replace_preserving_properties(h, before.epoch, 0..2, "AΒ", &cx).unwrap();
        let after = g.read(h, &cx).unwrap();
        assert_eq!(after.epoch, before.epoch + 1);
        assert_eq!(after.document.to_text(), "AΒ tail");
        assert_eq!(after.document.properties().at(CharPos(0)), Some(PropSetId(a.id())));
        assert_eq!(after.document.properties().at(CharPos(1)), Some(PropSetId(b.id())));
        assert!(g.replace_preserving_properties(h, before.epoch, 0..2, "bad", &cx).is_err());
        let mut host = g.host(&cx).unwrap();
        host.execute(Command::SwitchBuffer(h.buffer)).unwrap();
        host.execute(Command::Undo).unwrap();
        assert_eq!(g.read(h, &cx).unwrap().document.to_text(), "aβ tail");
        host.execute(Command::Redo).unwrap();
        assert_eq!(g.read(h, &cx).unwrap().document.to_text(), "AΒ tail");
    }
    #[test]
    fn expanding_case_replacement_shifts_following_properties_in_one_edit() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("expand", "ß tail", &cx).unwrap();
        let before = g.read(h, &cx).unwrap();
        // SAFETY: fresh scalar value in this Context.
        let style = unsafe { g.intern_property(cx.add("following"), &cx) }.unwrap();
        let prepared = g.prepare_properties(&before, CharPos(2)..CharPos(6), &style, &cx).unwrap();
        g.publish_properties(prepared, &cx).unwrap();
        let before = g.read(h, &cx).unwrap();
        g.replace_preserving_properties(h, before.epoch, 0..1, "SS", &cx).unwrap();
        let after = g.read(h, &cx).unwrap();
        assert_eq!(after.document.to_text(), "SS tail");
        assert_eq!(after.document.properties().at(CharPos(2)), Some(PropSetId(0)));
        assert_eq!(after.document.properties().at(CharPos(3)), Some(PropSetId(style.id())));
    }
}
#[cfg(test)]
mod case_coordinate_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    #[test]
    fn same_width_case_and_history_preserve_current_live_markers() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        let h = g.create("case-markers", "abc tail", &cx).unwrap();
        let old = g.register_marker(h, 1, Bias::Left, &cx).unwrap();
        let removed = g.register_marker(h, 2, Bias::Right, &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        g.replace_preserving_properties(h, read.epoch, 0..3, "ABC", &cx).unwrap();
        assert_eq!(g.marker_position(h, old, &cx).unwrap(), Some(1));
        g.remove_marker(h, removed, &cx).unwrap();
        let fresh = g.register_marker(h, 2, Bias::Left, &cx).unwrap();
        let mut native = g.host(&cx).unwrap();
        native.execute(Command::SwitchBuffer(h.buffer)).unwrap();
        for _ in 0..3 {
            native.execute(Command::Undo).unwrap();
            assert_eq!(g.read(h, &cx).unwrap().document.to_text(), "abc tail");
            assert_eq!(g.marker_position(h, fresh, &cx).unwrap(), Some(2));
            assert_eq!(g.marker_position(h, old, &cx).unwrap(), Some(1));
            assert_eq!(g.marker_position(h, removed, &cx).unwrap(), None);
            native.execute(Command::Redo).unwrap();
            assert_eq!(g.read(h, &cx).unwrap().document.to_text(), "ABC tail");
            assert_eq!(g.marker_position(h, fresh, &cx).unwrap(), Some(2));
            assert_eq!(g.marker_position(h, removed, &cx).unwrap(), None);
        }
    }
}

/// Scoped persistent interpreter owner. Env is rooted before this wrapper is
/// constructed, and neither rooted Env nor Context can escape its factory.
pub(crate) struct LispRuntimeHost<'a, 'roots, 'env> {
    owner: MutableGatewayEditorHost<'a, 'roots>,
    env: &'a mut crate::core::gc::Rt<crate::core::env::Env<'env>>,
}
impl LispRuntimeHost<'_, '_, '_> {
    /// Native command entry binds evaluator current-buffer to the selected UI
    /// view ONCE. Nested set-buffer/save-current-buffer remain evaluator-local.
    /// Partial Lisp effects on signal are retained; never replay this callback.
    pub(crate) fn evaluate_source(&mut self, source: &str) -> Result<(), String> {
        use rune_editor::EditorHost;
        let snapshot = self.owner.snapshot().map_err(|e| e.to_string())?;
        let selected = snapshot
            .views
            .iter()
            .find(|v| v.id == snapshot.selected_window)
            .ok_or_else(|| "Missing selected editor view".to_owned())?
            .view
            .buffer;
        drop(snapshot); // Do not pin every document/lease throughout evaluation.
        let handle = self
            .owner
            .gateway
            .inner
            .try_borrow()
            .map_err(|e| e.to_string())?
            .buffer_handle(selected, self.owner.cx)
            .map_err(|e| e.to_string())?;
        self.env.set_current_buffer(handle, self.owner.cx).map_err(|e| e.to_string())?;
        crate::lread::load_internal(source, self.owner.cx, self.env)
            .map(|_| ())
            .map_err(|error| {
                // Preserve the structured QUIT identity without printing an
                // arbitrary Lisp payload or reconstituting a condition by text.
                if let Some(error) = error.downcast_ref::<crate::eval::EvalError>() {
                    if let crate::eval::ErrorType::Signal(exit) = &error.error {
                        if exit
                            .bind(self.owner.cx)
                            .is_ok_and(|condition| condition.car() == crate::core::env::sym::QUIT)
                        {
                            return "quit".to_owned();
                        }
                    }
                }
                error.to_string()
            })
    }
}
impl rune_editor::EditorHost for LispRuntimeHost<'_, '_, '_> {
    type Doc = RuntimeDocument;
    fn snapshot(&self) -> rune_editor::Result<rune_editor::SessionSnapshot<RuntimeDocument>> {
        use rune_editor::EditorHost;
        self.owner.snapshot()
    }
    fn display_style_revision(&self) -> rune_editor::Result<rune_editor::DisplayStyleRevision> {
        Ok(rune_editor::DisplayStyleRevision {
            epoch: self.env.font_selection.generation(),
            frame_id: self.owner.cx.selected_frame().map(|f|f.id()).unwrap_or(0),
            default_face: crate::face_attributes::resolved_default_face(self.env, self.owner.cx),
        })
    }
    fn display_window_style(&self, window: rune_editor::WindowId, buffer: rune_editor::BufferId, len: usize) -> rune_editor::Result<rune_editor::WindowStyle> {
        use rune_editor::EditorHost;
        let snapshot = self.owner.snapshot()?;
        let view = snapshot.views.iter().find(|v|v.id == window && v.view.buffer == buffer).ok_or(rune_editor::EditorError::InvalidWindow)?;
        let doc = snapshot.buffers.iter().find(|b|b.id == view.view.buffer).ok_or(rune_editor::EditorError::InvalidBuffer)?;
        if doc.document.len_chars() != len { return Err(rune_editor::EditorError::InvalidPosition); }
        let face = crate::face_attributes::resolved_default_face(self.env, self.owner.cx).map_err(|e|rune_editor::EditorError::Backend(e.into()))?;
        Ok(rune_editor::WindowStyle {
            default_face: face,
            spans: if len == 0 {vec![]} else {vec![rune_editor::display::FaceSpan { range: 0..len, face }]},
        })
    }
    fn execute(
        &mut self,
        c: rune_editor::Command,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        use rune_editor::EditorHost;
        self.owner.execute(c)
    }
    fn dispatch_key(&mut self, k: rune_editor::Key) -> rune_editor::Result<rune_editor::Dispatch> {
        use rune_editor::EditorHost;
        self.owner.dispatch_key(k)
    }
    fn cancel_prefix(&mut self) -> rune_editor::Result<()> {
        use rune_editor::EditorHost;
        self.owner.cancel_prefix()
    }
    fn scroll_window(&mut self,w:rune_editor::WindowId,b:rune_editor::BufferId,sequence:u64,lines:i64)->rune_editor::Result<bool> {
        rune_editor::EditorHost::scroll_window(&mut self.owner,w,b,sequence,lines)
    }
    fn place_pointer(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        p: usize,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        use rune_editor::EditorHost;
        self.owner.place_pointer(w, b, e, p)
    }
    fn commit_replacement(
        &mut self,
        w: rune_editor::WindowId,
        b: rune_editor::BufferId,
        e: u64,
        r: Range<usize>,
        t: String,
    ) -> rune_editor::Result<rune_editor::CommandOutcome> {
        use rune_editor::EditorHost;
        self.owner.commit_replacement(w, b, e, r, t)
    }
}
pub(crate) fn with_lisp_runtime_owner<R>(
    width: u32,
    height: u32,
    run: impl for<'a, 'roots, 'env> FnOnce(LispRuntimeHost<'a, 'roots, 'env>) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    use crate::core::env::{Env, intern, sym};
    use rune_core::macros::root;
    use rune_editor::EditorHost;
    let roots = crate::core::gc::RootSet::default();
    let mut cx = Context::new(&roots);
    root!(env, init(Env::new(&cx)?), cx);
    sym::init_symbols();
    crate::core::env::init_variables(&cx, env);
    crate::data::defalias(intern("not", &cx), sym::NULL.into(), None)?;
    // Initialize the real owner frame before callback-free style snapshots.
    cx.selected_frame()?;
    let gateway = cx.editor_gateway()?;
    let mut host =
        LispRuntimeHost { owner: MutableGatewayEditorHost { gateway, cx: &mut cx }, env };
    host.execute(rune_editor::Command::Resize { width, height })?;
    run(host)
}
#[cfg(test)]
mod persistent_lisp_owner_tests {
    use super::*;
    use rune_editor::{Command, Document, EditorHost};
    #[test]
    fn persistent_lisp_environment_and_native_undo_share_one_session() {
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(setq owner-counter 40)").map_err(anyhow::Error::msg)?;
            host.evaluate_source("(setq owner-counter (+ owner-counter 2)) (if (= owner-counter 42) (insert \"λ🙂\") (error \"lost env\"))").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(),"λ🙂");host.execute(Command::Undo)?;assert_eq!(host.snapshot()?.buffers[0].document.to_text(),"");host.execute(Command::Redo)?;assert_eq!(host.snapshot()?.buffers[0].document.to_text(),"λ🙂");
            assert!(host.evaluate_source("(insert \"x\") (error \"signal after edit\")").is_err());assert_eq!(host.snapshot()?.buffers[0].document.to_text(),"λ🙂x");Ok(())
        }).unwrap();
    }
}
#[cfg(test)]
mod lisp_native_entry_sync_tests {
    use super::*;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn native_entry_sync_is_once_per_request_and_nested_set_buffer_stays_local() {
        with_lisp_runtime_owner(80,24,|mut host|{
            let before=host.snapshot()?;let selected=before.selected_window;let visible=before.views.iter().find(|v|v.id==selected).unwrap().view.buffer;
            host.evaluate_source("(setq owner-bg (get-buffer-create \"owner-background\")) (set-buffer owner-bg) (insert \"hidden\")").map_err(anyhow::Error::msg)?;
            let after=host.snapshot()?;assert_eq!(after.selected_window,selected);assert_eq!(after.views.iter().find(|v|v.id==selected).unwrap().view.buffer,visible);
            assert_eq!(after.buffers.iter().find(|b|b.id==visible).unwrap().document.to_text(),"");assert_eq!(after.buffers.iter().find(|b|b.name=="owner-background").unwrap().document.to_text(),"hidden");
            host.evaluate_source("(insert \"visible\") (save-current-buffer (set-buffer owner-bg) (insert \"!\")) (insert \".\")").map_err(anyhow::Error::msg)?;
            let after=host.snapshot()?;assert_eq!(after.buffers.iter().find(|b|b.id==visible).unwrap().document.to_text(),"visible.");assert_eq!(after.buffers.iter().find(|b|b.name=="owner-background").unwrap().document.to_text(),"hidden!");Ok(())
        }).unwrap();
    }
}

#[cfg(test)]
mod persistent_env_gc_tests {
    use super::*;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn persistent_rooted_env_survives_gc_between_owner_requests() {
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(setq owner-persistent-state (list \"survives-owner-gc\"))").map_err(anyhow::Error::msg)?;
            host.owner.with_context(|cx,_|{cx.garbage_collect(true);Ok(())})?;
            host.evaluate_source("(if (equal (car owner-persistent-state) \"survives-owner-gc\") (insert \"rooted\") (error \"lost rooted env\"))").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(),"rooted");Ok(())
        }).unwrap();
    }
}
impl RuntimeEditorGateway {
    pub(crate) fn modified(&self, handle: OwnerBufferHandle, cx: &Context) -> anyhow::Result<bool> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        owner.properties.empty(cx)?;
        anyhow::ensure!(handle.owner == cx.domain_id(), "Foreign editor owner");
        // Session authenticates membership in its live map or native tombstone
        // map. Read/edit/rename/kill still require a live handle.
        Ok(owner.session.modified(handle.buffer)?)
    }
    pub(crate) fn set_modified(
        &self,
        handle: OwnerBufferHandle,
        value: bool,
        cx: &Context,
    ) -> anyhow::Result<()> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.set_modified(id, value)?)
    }
}

impl LispRuntimeHost<'_, '_, '_> {
    /// Drain only currently queued native process events on this owner thread.
    /// Unlike a user command, polling preserves the evaluator's current-buffer
    /// binding. Filters select their own destination; no editor borrow spans Lisp.
    /// The bool reports output delivery, not pending work or process readiness.
    /// Callback effects survive errors: callers must publish changed snapshots
    /// even on Err and must never replay a consumed event.
    pub(crate) fn poll_process_events(&mut self) -> Result<bool, String> {
        crate::process_events::poll_process_events(self.env, self.owner.cx).map_err(|error| {
            if let Some(error) = error.downcast_ref::<crate::eval::EvalError>() {
                if let crate::eval::ErrorType::Signal(exit) = &error.error {
                    if exit
                        .bind(self.owner.cx)
                        .is_ok_and(|condition| condition.car() == crate::core::env::sym::QUIT)
                    {
                        return "quit".to_owned();
                    }
                }
            }
            error.to_string()
        })
    }

    /// Scoped token applies to Lisp callback safe points, not blocking foreign IO.
    pub(crate) fn poll_process_events_with_quit(
        &mut self,
        token: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<bool, String> {
        let _quit = self.env.install_quit_token(token);
        self.poll_process_events()
    }

    /// Fresh request-scoped interruption token. Guard installation owns no Env
    /// borrow and restores the previous token on success, signal or Rust unwind.
    /// This does not roll back earlier Lisp effects or bound blocking foreign calls.
    pub(crate) fn evaluate_source_with_quit(
        &mut self,
        source: &str,
        token: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        let _quit = self.env.install_quit_token(token);
        self.evaluate_source(source)
    }
}
#[cfg(test)]
mod persistent_lisp_quit_adapter_tests {
    use super::*;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn requested_token_reports_quit_and_does_not_poison_next_request() {
        with_lisp_runtime_owner(80, 24, |mut host| {
            let requested = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let error = host.evaluate_source_with_quit("(while t)", requested).unwrap_err();
            assert!(error.to_lowercase().contains("quit"), "unexpected error: {error}");
            host.evaluate_source("(insert \"after\")").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "after");
            Ok(())
        })
        .unwrap();
    }
}

mod save;
pub(crate) use save::{PendingSave, SaveKind, SaveMode, SaveOutcome, WrittenSave};

impl RuntimeEditorGateway {
    pub(crate) fn insert_before_markers(&self,handle:OwnerBufferHandle,epoch:u64,at:usize,text:&str,cx:&Context)->anyhow::Result<()> {
        self.check(cx)?;let mut owner=self.inner.try_borrow_mut()?;let id=owner.validate_handle(handle,cx)?;
        owner.session.insert_buffer_before_markers(id,epoch,at,text)?;Ok(())
    }
}

#[cfg(test)]
mod insert_before_marker_owner_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command,EditorHost};
    #[test]
    fn atomic_owner_insertion_preserves_properties_and_dynamic_live_marker_history() {
        let roots=RootSet::default();let cx=Context::new(&roots);let g=cx.editor_gateway().unwrap();let h=g.create("forced-markers","ab",&cx).unwrap();
        let read=g.read(h,&cx).unwrap();
        // SAFETY: fresh value belongs to this owner Context.
        let style=unsafe{g.intern_property(cx.add("face"),&cx)}.unwrap();
        let properties=g.prepare_properties(&read,CharPos(1)..CharPos(2),&style,&cx).unwrap();g.publish_properties(properties,&cx).unwrap();
        let left=g.register_marker(h,1,Bias::Left,&cx).unwrap();let right=g.register_marker(h,1,Bias::Right,&cx).unwrap();let removed=g.register_marker(h,1,Bias::Left,&cx).unwrap();let read=g.read(h,&cx).unwrap();
        g.insert_before_markers(h,read.epoch,1,"λ🙂",&cx).unwrap();
        assert_eq!(g.read(h,&cx).unwrap().document.to_text(),"aλ🙂b");assert_eq!(g.marker_position(h,left,&cx).unwrap(),Some(3));assert_eq!(g.marker_position(h,right,&cx).unwrap(),Some(3));assert_eq!(g.read(h,&cx).unwrap().document.properties().at(CharPos(3)),Some(PropSetId(style.id())));
        assert!(g.insert_before_markers(h,read.epoch,1,"stale",&cx).is_err());g.remove_marker(h,removed,&cx).unwrap();
        let mut host=g.host(&cx).unwrap();host.execute(Command::SwitchBuffer(h.buffer)).unwrap();host.execute(Command::Undo).unwrap();assert_eq!(g.marker_position(h,left,&cx).unwrap(),Some(1));
        let fresh=g.register_marker(h,1,Bias::Left,&cx).unwrap();
        host.execute(Command::Redo).unwrap();assert_eq!(g.marker_position(h,left,&cx).unwrap(),Some(3));assert_eq!(g.marker_position(h,right,&cx).unwrap(),Some(3));assert_eq!(g.marker_position(h,fresh,&cx).unwrap(),Some(1));assert_eq!(g.marker_position(h,removed,&cx).unwrap(),None);
        assert_eq!(g.read(h,&cx).unwrap().document.properties().at(CharPos(3)),Some(PropSetId(style.id())));
    }
}

#[cfg(test)]
mod scoped_scroll_owner_tests {
 use super::*;use rune_editor::{EditorHost,Command};
 #[test]
 fn actual_owner_scroll_is_read_only_safe_and_does_not_change_evaluator_point() {
  with_lisp_runtime_owner(80,24,|mut host|{
   host.evaluate_source("(insert \"first λ\\nsecond🙂\\nthird\\n\")").unwrap();let before=host.snapshot()?;let v=before.views.iter().find(|v|v.id==before.selected_window).unwrap();let point=v.view.point;let b=v.view.buffer;let w=v.id;
   assert!(host.scroll_window(w,b,before.sequence,1)?);let after=host.snapshot()?;let v=after.views.iter().find(|v|v.id==w).unwrap();assert_eq!(v.view.start,8);assert_eq!(v.view.point,point);assert_eq!(before.buffers[0].version,after.buffers[0].version);assert!(host.scroll_window(w,b,before.sequence,1).is_err());host.execute(Command::Undo)?;assert_eq!(host.snapshot()?.buffers.iter().find(|b2|b2.id==b).unwrap().document.to_text(),"");Ok(())
  }).unwrap();
 }
}

impl RuntimeEditorGateway {
    pub(crate) fn insert_preserving_point(&self,handle:OwnerBufferHandle,epoch:u64,at:usize,text:&str,cx:&Context)->anyhow::Result<()> {
        self.check(cx)?;let mut owner=self.inner.try_borrow_mut()?;let id=owner.validate_handle(handle,cx)?;
        owner.session.insert_buffer_preserving_point(id,epoch,at,text)?;Ok(())
    }
}

impl RuntimeEditorGateway {
    /// Native boolean policy writes canonical t/nil, never leaves an old raw
    /// Lisp truthy value attached to the policy it superseded.
    pub(crate) fn set_read_only(&self,handle:OwnerBufferHandle,value:bool,cx:&Context)->anyhow::Result<()> {
        // SAFETY: bool conversion produces audited global t/nil only.
        unsafe {self.set_read_only_value(handle,Object::from(value),cx)}
    }
    pub(crate) fn read_only_value<'ob>(&self,handle:OwnerBufferHandle,cx:&'ob Context)->anyhow::Result<Object<'ob>> {
        self.check(cx)?;let mut owner=self.inner.try_borrow_mut()?;let id=owner.validate_handle(handle,cx)?;
        if !owner.session.buffer_snapshot(id)?.read_only {
            owner.read_only_values.remove(&id);return Ok(Object::from(false));
        }
        match owner.read_only_values.get(&id) {Some(value)=>value.bind(cx),None=>Ok(Object::from(true))}
    }
    /// Restore one traced dynamic binding to its ORIGINAL target. Only a known
    /// killed buffer is skipped; busy/foreign/unknown owner errors propagate.
    /// SAFETY: same owned reachable-graph/rooting contract as set_read_only_value.
    pub(crate) unsafe fn restore_read_only_value(&self,handle:OwnerBufferHandle,value:Object,cx:&Context)->anyhow::Result<()> {
        self.check(cx)?;
        {
            let owner=self.inner.try_borrow()?;owner.properties.empty(cx)?;
            anyhow::ensure!(handle.owner==cx.domain_id(),"Foreign readonly binding target");
            if !owner.session.buffer_live(handle.buffer) {
                owner.session.modified(handle.buffer)?; // Authenticated tombstone, not unknown ID.
                return Ok(());
            }
        }
        unsafe{self.set_read_only_value(handle,value,cx)}
    }
    /// SAFETY: every reachable local allocation in value belongs to cx (or
    /// audited global storage) and is rooted by the caller until registration.
    /// Domain checks authenticate destination, NOT arbitrary payload provenance.
    pub(crate) unsafe fn set_read_only_value(&self,handle:OwnerBufferHandle,value:Object,cx:&Context)->anyhow::Result<()> {
        unsafe {self.set_read_only_value_with_policy(handle,value,cx,|owner,id,enabled|owner.session.set_read_only(id,enabled))}
    }
    unsafe fn set_read_only_value_with_policy(&self,handle:OwnerBufferHandle,value:Object,cx:&Context,apply:impl FnOnce(&mut RuntimeEditor,rune_editor::BufferId,bool)->rune_editor::Result<()>)->anyhow::Result<()> {
        self.check(cx)?;
        {self.inner.try_borrow()?.validate_handle(handle,cx)?;}
        let enabled=!value.is_nil();
        // Mint before borrowing the owner; HeapDomainValue contains traced
        // owner slots, never an unrooted Object<'static> in Session/snapshots.
        let retained=if enabled {Some(Arc::new(unsafe{crate::core::object::HeapDomainValue::new(value,cx)}))}else{None};
        let mut owner=self.inner.try_borrow_mut()?;let id=owner.validate_handle(handle,cx)?;
        if let Some(retained)=retained {
            // Allocate the native map entry before committing Session metadata.
            // Checked sequence failure restores the prior root without callbacks.
            let old=owner.read_only_values.insert(id,retained);
            if let Err(error)=apply(&mut owner,id,true) {
                if let Some(old)=old {owner.read_only_values.insert(id,old);}else{owner.read_only_values.remove(&id);}
                return Err(error.into());
            }
        } else {
            apply(&mut owner,id,false)?;
            owner.read_only_values.remove(&id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod raw_readonly_owner_tests {
 use super::*;use crate::core::gc::RootSet;use rune_editor::{EditorHost,Command};
 #[test]
 fn raw_truthy_value_survives_gc_native_bool_canonicalizes_and_kill_releases() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);let g=cx.editor_gateway().unwrap();let h=g.create("readonly-root","text",cx).unwrap();
  // SAFETY: fresh owner allocation, retained before any GC.
  unsafe{g.set_read_only_value(h,cx.add("original truthy"),cx)}.unwrap();cx.garbage_collect(true);
  assert_eq!(g.read_only_value(h,cx).unwrap(),"original truthy");let before=g.read(h,cx).unwrap();assert!(g.splice(h,before.epoch,0..0,"blocked",cx).is_err());
  g.set_read_only(h,true,cx).unwrap();assert_eq!(g.read_only_value(h,cx).unwrap(),Object::from(true));g.set_read_only(h,false,cx).unwrap();assert!(g.read_only_value(h,cx).unwrap().is_nil());assert!(!g.inner.borrow().read_only_values.contains_key(&h.buffer));
  // Zero is non-nil in Lisp and must remain exact, not normalized to t.
  unsafe{g.set_read_only_value(h,0i64.into(),cx)}.unwrap();assert_eq!(g.read_only_value(h,cx).unwrap(),Object::from(0i64));
  g.host(cx).unwrap().execute(Command::KillBuffer{buffer:h.buffer,force:true}).unwrap();assert!(!g.inner.borrow().read_only_values.contains_key(&h.buffer));assert!(g.read_only_value(h,cx).is_err());
 }
 #[test]
 fn foreign_owner_rejected_and_truthy_replacement_does_not_leak_old_root() {
  let roots=RootSet::default();let cx=Context::new(&roots);let g=cx.editor_gateway().unwrap();let h=g.create("raw","",&cx).unwrap();
  unsafe{g.set_read_only_value(h,cx.add("one"),&cx)}.unwrap();let old=Arc::downgrade(g.inner.borrow().read_only_values.get(&h.buffer).unwrap());unsafe{g.set_read_only_value(h,cx.add("two"),&cx)}.unwrap();assert!(old.upgrade().is_none());assert_eq!(g.read_only_value(h,&cx).unwrap(),"two");
  drop(cx);let other_roots=RootSet::default();let other=Context::new(&other_roots);assert!(g.read_only_value(h,&other).is_err());assert!(g.set_read_only(h,false,&other).is_err());
 }
 #[test]
 fn dynamic_restore_targets_original_live_buffer_and_only_skips_known_dead() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);let g=cx.editor_gateway().unwrap();let a=g.create("binding-a","",cx).unwrap();let b=g.create("binding-b","",cx).unwrap();
  unsafe{g.set_read_only_value(a,cx.add("original"),cx)}.unwrap();let saved=g.inner.borrow().read_only_values.get(&a.buffer).unwrap().clone();g.set_read_only(a,false,cx).unwrap();g.host(cx).unwrap().execute(Command::SwitchBuffer(b.buffer)).unwrap();cx.garbage_collect(true);unsafe{g.restore_read_only_value(a,saved.bind(cx).unwrap(),cx)}.unwrap();assert_eq!(g.read_only_value(a,cx).unwrap(),"original");assert!(g.read_only_value(b,cx).unwrap().is_nil());
  {let _busy=g.inner.borrow_mut();assert!(unsafe{g.restore_read_only_value(a,Object::from(false),cx)}.is_err());}
  g.kill(a,true,cx).unwrap();assert!(unsafe{g.restore_read_only_value(a,saved.bind(cx).unwrap(),cx)}.is_ok());
  let other=RuntimeEditorGateway::new(cx,80,24).unwrap();let foreign=other.create("foreign","",cx).unwrap();assert!(unsafe{g.restore_read_only_value(foreign,Object::from(false),cx)}.is_err());
 }

 #[test]
 fn checked_policy_failure_rolls_back_raw_root_and_nil_does_not_drop_original() {
  let roots=RootSet::default();let cx=Context::new(&roots);let g=cx.editor_gateway().unwrap();let h=g.create("rollback","",&cx).unwrap();let seq=g.host(&cx).unwrap().snapshot().unwrap().sequence;
  let fail=|_:&mut RuntimeEditor,_:rune_editor::BufferId,_:bool|Err(rune_editor::EditorError::IdExhausted);
  assert!(unsafe{g.set_read_only_value_with_policy(h,cx.add("new"),&cx,fail)}.is_err());assert!(g.read_only_value(h,&cx).unwrap().is_nil());assert!(g.inner.borrow().read_only_values.is_empty());
  unsafe{g.set_read_only_value(h,cx.add("original"),&cx)}.unwrap();let original=g.inner.borrow().read_only_values.get(&h.buffer).unwrap().clone();
  assert!(unsafe{g.set_read_only_value_with_policy(h,cx.add("replacement"),&cx,fail)}.is_err());assert!(Arc::ptr_eq(&original,g.inner.borrow().read_only_values.get(&h.buffer).unwrap()));assert_eq!(g.read_only_value(h,&cx).unwrap(),"original");
  assert!(unsafe{g.set_read_only_value_with_policy(h,Object::from(false),&cx,fail)}.is_err());assert_eq!(g.read_only_value(h,&cx).unwrap(),"original");assert_eq!(g.host(&cx).unwrap().snapshot().unwrap().sequence,seq+1);
 }

}

impl LispRuntimeHost<'_, '_, '_> {
    pub(crate) fn dispatch_native_key(
        &mut self,
        window_id: u64,
        buffer_id: u64,
        key: &str,
        control: bool,
        meta: bool,
        shift: bool,
    ) -> Result<bool, String> {
        use rune_editor::EditorHost;
        if (control && !meta && key == "g") || (!control && !meta && key == "escape") {
            crate::keymap::cancel_native_prefix(self.env);
            return Ok(true);
        }
        let snapshot = self.owner.snapshot().map_err(|e| e.to_string())?;
        let view = snapshot
            .views
            .iter()
            .find(|v| v.id == snapshot.selected_window)
            .ok_or("Missing selected editor view")?;
        if view.id.as_u64() != window_id || view.view.buffer.as_u64() != buffer_id {
            return Err("Stale native key target".into());
        }
        let (selected, sequence) = (view.view.buffer, snapshot.sequence);
        drop(snapshot);
        let handle = self
            .owner
            .gateway
            .inner
            .try_borrow()
            .map_err(|e| e.to_string())?
            .buffer_handle(selected, self.owner.cx)
            .map_err(|e| e.to_string())?;
        self.env.set_current_buffer(handle, self.owner.cx).map_err(|e| e.to_string())?;
        let encoded=crate::keymap::encode_native_key(key,control,meta,shift);
        let event=match &encoded {crate::keymap::EncodedNativeKey::Character(code)=>crate::keymap::NativeEvent::Character(*code),crate::keymap::EncodedNativeKey::Named(name)=>crate::keymap::NativeEvent::Named(name)};
        crate::keymap::dispatch_native_event(event, sequence, self.env, self.owner.cx)
            .map(|outcome| outcome != crate::keymap::NativeDispatch::Unbound)
            .map_err(|e| e.to_string())
    }
}
impl LispRuntimeHost<'_, '_, '_> {
    pub(crate) fn dispatch_native_key_with_quit(
        &mut self,
        window: u64,
        buffer: u64,
        key: &str,
        control: bool,
        meta: bool,
        shift: bool,
        token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<bool, String> {
        let _scope = self.env.install_quit_token(token);
        self.dispatch_native_key(window, buffer, key, control, meta, shift)
    }
}

#[cfg(test)]
mod owner_process_poll_adapter_tests {
    use super::*;
    use rune_editor::EditorHost;
    #[test]
    fn real_filter_partial_effects_survive_error_without_replay() {
        use rune_editor::Document;
        with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source(r#"(setq poll-bg (get-buffer-create "poll-callback"))
                (set-buffer poll-bg)
                (setq poll-process (start-process "poll-host-child" nil "/usr/bin/python3" "-c" "import os; os.write(1,b'x')"))
                (set-process-coding-system poll-process 'utf-8-unix 'utf-8-unix)
                (set-process-filter poll-process #'(lambda (p text) (insert text) (garbage-collect) (error "poll-filter-failed")))"#)
                .map_err(anyhow::Error::msg)?;
            let before = host.snapshot()?.sequence;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let error = loop {
                match host.poll_process_events() {
                    Err(error) => break error,
                    Ok(_) => {
                        anyhow::ensure!(std::time::Instant::now() < deadline, "child output deadline");
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            };
            assert!(error.contains("poll-filter-failed") || error.contains("Signal"), "{error}");
            let after = host.snapshot()?;
            assert!(after.sequence > before);
            assert_eq!(after.buffers.iter().find(|b| b.name == "poll-callback").unwrap().document.to_text(), "x");
            drop(after);
            for _ in 0..4 { let _ = host.poll_process_events(); }
            assert_eq!(host.snapshot()?.buffers.iter().find(|b| b.name == "poll-callback").unwrap().document.to_text(), "x");
            host.evaluate_source("(insert \"next\")").map_err(anyhow::Error::msg)?;
            Ok(())
        }).unwrap();
    }
    #[test]
    fn idle_poll_preserves_evaluator_binding_and_releases_scoped_token() {
        with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source(
                "(setq poll-bg (get-buffer-create \"poll-background\")) (set-buffer poll-bg)",
            )
            .map_err(anyhow::Error::msg)?;
            let before = host.snapshot()?.sequence;
            assert!(!host.poll_process_events().map_err(anyhow::Error::msg)?);
            assert!(
                !host
                    .poll_process_events_with_quit(Arc::new(std::sync::atomic::AtomicBool::new(
                        false
                    )))
                    .map_err(anyhow::Error::msg)?
            );
            assert_eq!(host.poll_process_events_with_quit(Arc::new(
                std::sync::atomic::AtomicBool::new(true))).unwrap_err(), "quit");
            assert!(!host.poll_process_events().map_err(anyhow::Error::msg)?);
            assert_eq!(host.snapshot()?.sequence, before);
            crate::lread::load_internal(
                "(if (eq (current-buffer) poll-bg) t (error \"poll retargeted Env\"))",
                host.owner.cx,
                host.env,
            )?;
            host.evaluate_source("(insert \"next-request\")").map_err(anyhow::Error::msg)?;
            Ok(())
        })
        .unwrap();
    }
}

impl RuntimeEditorGateway {
    /// Native-only witness. This is not Context/domain authorization.
    pub(crate) fn buffer_liveness(&self, handle: OwnerBufferHandle, cx: &Context)
        -> anyhow::Result<rune_editor::BufferLiveness> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.buffer_liveness(id)?)
    }
    pub(crate) fn register_marker_pair(&self, handle: OwnerBufferHandle,
        start: usize, start_bias: Bias, end: usize, end_bias: Bias, cx: &Context,
    ) -> anyhow::Result<(rune_editor::MarkerId, rune_editor::MarkerId)> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.register_marker_pair(id, start, start_bias, end, end_bias)?)
    }
    pub(crate) fn marker_pair_positions(&self, handle: OwnerBufferHandle,
        pair: (rune_editor::MarkerId, rune_editor::MarkerId), cx: &Context,
    ) -> anyhow::Result<Option<(usize, usize)>> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        let id = owner.validate_handle(handle, cx)?;
        let first = owner.session.marker_position(id, pair.0)?;
        let second = owner.session.marker_position(id, pair.1)?;
        Ok(first.zip(second))
    }
    pub(crate) fn move_marker_pair(&self, handle: OwnerBufferHandle,
        pair: (rune_editor::MarkerId, rune_editor::MarkerId), start: usize,
        start_bias: Bias, end: usize, end_bias: Bias, cx: &Context,
    ) -> anyhow::Result<()> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.move_marker_pair(id, pair, start, start_bias, end, end_bias)?)
    }
    pub(crate) fn transfer_marker_pair(&self, source: OwnerBufferHandle, target: OwnerBufferHandle,
        pair: (rune_editor::MarkerId, rune_editor::MarkerId), start: usize,
        start_bias: Bias, end: usize, end_bias: Bias, cx: &Context,
    ) -> anyhow::Result<rune_editor::BufferLiveness> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let source = owner.validate_handle(source, cx)?;
        let target = owner.validate_handle(target, cx)?;
        Ok(owner.session.transfer_marker_pair(source, target, pair, start, start_bias, end, end_bias)?)
    }
    pub(crate) fn remove_marker_pair(&self, handle: OwnerBufferHandle,
        pair: (rune_editor::MarkerId, rune_editor::MarkerId), cx: &Context,
    ) -> anyhow::Result<()> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.remove_marker_pair(id, pair)?)
    }
}

#[cfg(test)]
mod overlay_pair_gateway_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn pairs_authenticate_both_buffers_and_do_not_retarget_evaluator_or_views() {
        let roots=RootSet::default();let cx=Context::new(&roots);
        let g=RuntimeEditorGateway::new(&cx,80,24).unwrap();let other=RuntimeEditorGateway::new(&cx,80,24).unwrap();
        let source=g.create("overlay source","λ🙂abc",&cx).unwrap();let target=g.create("overlay target","xy",&cx).unwrap();
        let foreign=other.create("foreign","foreign",&cx).unwrap();
        let read=g.read(source,&cx).unwrap();let token=g.buffer_liveness(source,&cx).unwrap();
        let pair=g.register_marker_pair(source,1,Bias::Left,3,Bias::Right,&cx).unwrap();
        assert_eq!(g.read(source,&cx).unwrap().epoch,read.epoch);
        assert!(g.transfer_marker_pair(source,foreign,pair,0,Bias::Left,1,Bias::Right,&cx).is_err());
        assert_eq!(g.marker_pair_positions(source,pair,&cx).unwrap(),Some((1,3)));
        assert!(g.move_marker_pair(source,pair,0,Bias::Left,99,Bias::Right,&cx).is_err());
        let target_live=g.transfer_marker_pair(source,target,pair,0,Bias::Right,2,Bias::Left,&cx).unwrap();
        assert!(target_live.is_live());assert_eq!(g.marker_pair_positions(source,pair,&cx).unwrap(),None);
        assert_eq!(g.marker_pair_positions(target,pair,&cx).unwrap(),Some((0,2)));
        assert_eq!(g.read(source,&cx).unwrap().epoch,read.epoch);
        g.remove_marker_pair(target,pair,&cx).unwrap();g.remove_marker_pair(target,pair,&cx).unwrap();
        assert_eq!(g.marker_pair_positions(target,pair,&cx).unwrap(),None);
        g.kill(source,true,&cx).unwrap();assert!(!token.is_live());assert!(target_live.is_live());
    }
}

#[cfg(test)]
mod overlay_handle_owner_stamp_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn owner_stamp_never_borrows_editor_and_is_not_liveness_authority() {
        let old_handle;
        {
            let roots=RootSet::default();let cx=Context::new(&roots);
            let gateway=RuntimeEditorGateway::new(&cx,80,24).unwrap();
            old_handle=gateway.create("stamp","",&cx).unwrap();
            let borrow=gateway.inner.borrow_mut();
            assert!(old_handle.belongs_to(&cx));
            drop(borrow);
            gateway.kill(old_handle,true,&cx).unwrap();
            assert!(old_handle.belongs_to(&cx)); // Stamp is deliberately not liveness.
            assert!(gateway.read(old_handle,&cx).is_err());
        }
        let roots=RootSet::default();let cx=Context::new(&roots);
        assert!(!old_handle.belongs_to(&cx));
    }
}

mod native_startup;
pub(crate) use native_startup::check_native_bootstrap;
