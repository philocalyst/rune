//! Owner-arena marker identity. Native positions are authoritative in Session;
//! this header never keeps a stale position mirror or an editor/document lease.
use super::{CloneIn, Gc, HeapDomainValue, IntoObject, TagType, WithLifetime};
use crate::{
    core::gc::{AllocState, Block, Context, GcHeap, GcState, OwnerLiveness, Trace},
    editor_document::{MarkerLifetime, OwnerBufferHandle},
};
use anyhow::{Result, ensure};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT_MARKER: AtomicU64 = AtomicU64::new(1);

/// Observation only. The exact native registration and its retirement duty are
/// private and cannot be manufactured by mutating this view.
#[derive(Clone)]
pub(crate) struct MarkerAttachmentView {
    buffer: OwnerBufferHandle,
    id: rune_editor::MarkerId,
    liveness: rune_editor::BufferLiveness,
    owner_liveness: OwnerLiveness,
}
impl MarkerAttachmentView {
    pub(crate) fn buffer(&self) -> OwnerBufferHandle {
        self.buffer
    }
    pub(crate) fn id(&self) -> rune_editor::MarkerId {
        self.id
    }
    pub(crate) fn is_live(&self) -> bool {
        self.liveness.is_live() && self.owner_liveness.is_live()
    }
}
pub(crate) struct MarkerAttachment {
    view: MarkerAttachmentView,
}
impl MarkerAttachment {
    /// SAFETY: all fields must be issued by ONE successful authenticated native
    /// marker transaction for this buffer/id using this marker's lifetime token.
    /// Caller prevalidates before native commit; no callback/GC may intervene.
    pub(crate) unsafe fn from_registration(
        buffer: OwnerBufferHandle,
        id: rune_editor::MarkerId,
        liveness: rune_editor::BufferLiveness,
        cx: &Context,
    ) -> Self {
        Self {
            view: MarkerAttachmentView {
                buffer,
                id,
                liveness,
                owner_liveness: cx.owner_liveness(),
            },
        }
    }
}
pub(crate) struct MarkerInner {
    id: u64,
    owner: u64,
    owner_liveness: OwnerLiveness,
    // Exactly one lifetime duty; remains through detachment and GC moves.
    lifetime: MarkerLifetime,
    insertion_type: Cell<bool>,
    attachment: RefCell<Option<MarkerAttachment>>,
}
pub(crate) struct LispMarker(pub(in crate::core) GcHeap<MarkerInner>);
impl LispMarker {
    pub(crate) fn new<'a>(cx: &'a Context<'_>) -> Result<Gc<&'a Self>> {
        let id = NEXT_MARKER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| x.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Marker identities exhausted"))?;
        let lifetime = cx.editor_gateway()?.new_marker_lifetime(cx)?;
        Ok(Self(GcHeap::new(
            MarkerInner {
                id,
                owner: cx.domain_id(),
                owner_liveness: cx.owner_liveness(),
                lifetime,
                insertion_type: Cell::new(false),
                attachment: RefCell::new(None),
            },
            false,
        ))
        .into_obj(&cx.block))
    }
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }
    pub(crate) fn validate(&self, cx: &Context) -> Result<()> {
        ensure!(
            self.0.owner == cx.domain_id()
                && self.0.owner_liveness.is_live()
                && cx.block.belongs_to_owner(self.0.owner),
            "Invalid Marker owner/lifetime"
        );
        Ok(())
    }
    /// Immutable native token borrow. Shared owner access excludes moving GC;
    /// the token exposes no mutable queue borrow and cannot be cloned.
    pub(crate) fn lifetime(&self, cx: &Context) -> Result<&MarkerLifetime> {
        self.validate(cx)?;
        Ok(&self.0.lifetime)
    }
    pub(crate) fn insertion_type(&self) -> bool {
        self.0.insertion_type.get()
    }
    pub(crate) fn set_insertion_type_native(&self, value: bool, cx: &Context) -> Result<()> {
        self.validate(cx)?;
        self.0.insertion_type.set(value);
        Ok(())
    }
    /// SAFETY: validate(cx) preceded an immediately successful native bias
    /// transaction; no callback/GC or owner retirement intervenes. This commit
    /// stores only the already-applied native bias and cannot fail afterward.
    pub(crate) unsafe fn set_insertion_type_prevalidated(&self, value: bool) {
        self.0.insertion_type.set(value);
    }
    pub(crate) fn attachment(&self, cx: &Context) -> Result<Option<MarkerAttachmentView>> {
        self.validate(cx)?;
        Ok(self
            .0
            .attachment
            .borrow()
            .as_ref()
            .map(|x| x.view.clone())
            .filter(MarkerAttachmentView::is_live))
    }
    /// SAFETY: validate(cx) and native attachment provenance were established
    /// before the immediately preceding native transaction. No callback or GC
    /// may intervene. No RefCell guard escapes any method of this private type,
    /// so replacement cannot encounter a live borrow. Attachment witnesses own
    /// no retirement duty: the immutable lifetime token stays in the header.
    pub(crate) unsafe fn install_attachment_prevalidated(
        &self,
        attachment: Option<MarkerAttachment>,
    ) {
        *self.0.attachment.borrow_mut() = attachment;
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            AllocState::Forwarded(p) => Some(p),
            AllocState::Unmoved => None,
            AllocState::Global => unreachable!("global Marker violates transfer invariant"),
        }
    }
}
impl Trace for MarkerInner {
    fn trace(&self, _state: &mut GcState) {}
}
impl<'new> CloneIn<'new, &'new Self> for LispMarker {
    fn clone_in<const C: bool>(&self, target: &'new Block<C>) -> Result<Gc<&'new Self>> {
        ensure!(
            target.belongs_to_owner(self.0.owner) && self.0.owner_liveness.is_live(),
            "Marker transfer across heap domains is unsupported"
        );
        // SAFETY: only the owning Context mints this arena brand; no foreign
        // destination reaches this identity rebind. Allocation outlives target.
        Ok(unsafe { self.with_lifetime().tag() })
    }
}
// Native identity, NOT a claim of GNU contextual marker-position `equal`.
impl PartialEq for LispMarker {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}
impl Eq for LispMarker {}
impl fmt::Debug for LispMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<marker:{}>", self.id())
    }
}
impl fmt::Display for LispMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
crate::derive_GcMoveable!(LispMarker);

struct CanonicalMark {
    value: Arc<HeapDomainValue>,
    origin_liveness: rune_editor::BufferLiveness,
}
#[derive(Default)]
pub(crate) struct MarkerRegistry {
    marks: HashMap<OwnerBufferHandle, CanonicalMark>,
}
impl MarkerRegistry {
    pub(in crate::core) fn prune_dead(&mut self) {
        self.marks.retain(|_, m| m.origin_liveness.is_live());
    }
    pub(in crate::core) fn shutdown(&mut self) {
        self.marks.clear();
    }
}
impl Context<'_> {
    /// Origin-buffer root is separate from current marker attachment: killing
    /// origin A must not detach an externally retained marker now targeting B.
    /// SAFETY: liveness is from authenticated buffer in this Context, same turn.
    pub(crate) unsafe fn mark_marker<'a>(
        &'a self,
        buffer: OwnerBufferHandle,
        liveness: rune_editor::BufferLiveness,
    ) -> Result<Gc<&'a LispMarker>> {
        ensure!(
            buffer.belongs_to(self) && liveness.is_live(),
            "Invalid mark-marker buffer owner/liveness"
        );
        if let Some(entry) = self.marker_registry.borrow().marks.get(&buffer) {
            let value = entry.value.bind(self)?;
            return value.try_into().map_err(anyhow::Error::from);
        }
        let marker = LispMarker::new(self)?;
        // SAFETY: fresh owner marker, no Object metadata and no GC/callback.
        let value = Arc::new(unsafe { HeapDomainValue::new(marker.into(), self) });
        let mut registry = self.marker_registry.try_borrow_mut()?;
        registry.marks.try_reserve(1)?;
        registry
            .marks
            .insert(buffer, CanonicalMark { value, origin_liveness: liveness });
        Ok(marker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        gc::RootSet,
        object::{Object, ObjectType},
    };
    use rune_core::macros::root;
    #[test]
    fn detached_marker_keeps_identity_and_bias_across_moving_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let marker = LispMarker::new(cx).unwrap();
        let id = marker.id();
        marker.set_insertion_type_native(true, cx).unwrap();
        root!(value, Object::from(marker), cx);
        for _ in 0..3 {
            let before = value.bind(cx).into_raw().addr();
            cx.garbage_collect(true);
            assert_ne!(value.bind(cx).into_raw().addr(), before);
            let ObjectType::Marker(marker) = value.bind(cx).untag() else {
                panic!("marker tag lost")
            };
            assert_eq!(marker.id(), id);
            assert!(marker.insertion_type());
            assert!(marker.attachment(cx).unwrap().is_none());
            let same = marker.clone_in(&cx.block).unwrap();
            assert_eq!(same.id(), id);
        }
    }
    #[test]
    fn marker_rejects_global_transfer_without_silent_shell() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let marker = LispMarker::new(cx).unwrap();
        let symbols = crate::core::env::INTERNED_SYMBOLS.lock().unwrap();
        assert!(marker.clone_in(symbols.global_block()).is_err());
        drop(symbols);
        let (sender, receiver) = crate::core::object::make_channel_pair(1);
        assert!(matches!(
            sender.try_send(marker.into()),
            Err(crate::channel_manager::SendError::CopyFailed)
        ));
        assert!(matches!(receiver.try_recv(cx), Err(crate::channel_manager::RecvError::Empty)));
        sender.close();
        receiver.close();
        assert!(marker.validate(cx).is_ok());
    }
    #[test]
    fn unrooted_marker_death_retires_native_registration_after_moving_survival() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let gateway = cx.editor_gateway().unwrap();
        let buffer = gateway.create("marker-gc", "abc", cx).unwrap();
        let native_id;
        {
            let marker = LispMarker::new(cx).unwrap();
            marker.validate(cx).unwrap();
            let registered = gateway
                .replace_marker_attachment(
                    marker.lifetime(cx).unwrap(),
                    buffer,
                    1,
                    rune_editor::Bias::Left,
                    cx,
                )
                .unwrap();
            native_id = registered.id;
            // SAFETY: exact authenticated registration, no callbacks/GC since validation.
            unsafe {
                marker.install_attachment_prevalidated(Some(MarkerAttachment::from_registration(
                    registered.buffer,
                    registered.id,
                    registered.liveness,
                    cx,
                )));
            }
            root!(marker, cx);
            for _ in 0..3 {
                cx.garbage_collect(true);
                assert_eq!(marker.bind(cx).attachment(cx).unwrap().unwrap().id(), native_id);
                assert_eq!(gateway.marker_position(buffer, native_id, cx).unwrap(), Some(1));
            }
        }
        cx.garbage_collect(true);
        assert_eq!(gateway.marker_position(buffer, native_id, cx).unwrap(), None);
        cx.garbage_collect(true);
        assert_eq!(gateway.marker_position(buffer, native_id, cx).unwrap(), None);
    }
    #[test]
    fn canonical_origin_root_does_not_detach_retained_marker_transferred_to_other_buffer() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let gateway = cx.editor_gateway().unwrap();
        let origin = gateway.create("mark-origin", "abc", cx).unwrap();
        let target = gateway.create("mark-target", "xyz", cx).unwrap();
        let origin_liveness = gateway.buffer_liveness(origin, cx).unwrap();
        // SAFETY: liveness is from the exact authenticated origin buffer.
        let marker = unsafe { cx.mark_marker(origin, origin_liveness.clone()).unwrap() };
        let id = marker.id();
        assert_eq!(unsafe { cx.mark_marker(origin, origin_liveness).unwrap() }.id(), id);
        marker.validate(cx).unwrap();
        let registered = gateway
            .replace_marker_attachment(
                marker.lifetime(cx).unwrap(),
                target,
                2,
                rune_editor::Bias::Left,
                cx,
            )
            .unwrap();
        let native_id = registered.id;
        // SAFETY: exact authenticated target registration in the same no-GC turn.
        unsafe {
            marker.install_attachment_prevalidated(Some(MarkerAttachment::from_registration(
                registered.buffer,
                registered.id,
                registered.liveness,
                cx,
            )));
        }
        root!(marker, cx);
        gateway.kill(origin, true, cx).unwrap();
        cx.garbage_collect(true);
        assert!(cx.marker_registry.borrow().marks.is_empty());
        assert_eq!(marker.bind(cx).id(), id);
        assert!(marker.bind(cx).attachment(cx).unwrap().unwrap().is_live());
        assert_eq!(gateway.marker_position(target, native_id, cx).unwrap(), Some(2));
    }
}
