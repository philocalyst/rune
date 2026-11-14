//! Mutable owner-arena overlays. Detached properties are ordinary traced edges,
//! not independently self-rooted values. Cross-domain copies fail explicitly.
use super::{
    CloneIn, Gc, HeapDomainValue, IntoObject, NIL, ObjCell, Object, TagType, WithLifetime,
};
use crate::{
    core::gc::{AllocState, Block, Context, GcHeap, GcState, OwnerLiveness, Trace},
    editor_document::OwnerBufferHandle,
};
use anyhow::{Result, ensure};
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT_OVERLAY: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub(crate) struct OverlayAttachment {
    buffer: OwnerBufferHandle,
    start: rune_editor::MarkerId,
    end: rune_editor::MarkerId,
    liveness: rune_editor::BufferLiveness,
    owner_liveness: OwnerLiveness,
    _prepared: (),
}
impl OverlayAttachment {
    pub(crate) fn buffer(&self) -> OwnerBufferHandle {
        self.buffer
    }
    pub(crate) fn start(&self) -> rune_editor::MarkerId {
        self.start
    }
    pub(crate) fn end(&self) -> rune_editor::MarkerId {
        self.end
    }
    pub(crate) fn liveness(&self) -> &rune_editor::BufferLiveness {
        &self.liveness
    }
    pub(crate) fn owner_liveness(&self) -> &OwnerLiveness {
        &self.owner_liveness
    }
    /// SAFETY: markers and liveness must come from the same authenticated
    /// gateway marker-pair transaction for buffer, with no intervening mutation.
    /// A token alone is observation, not buffer identity or access authority.
    pub(crate) unsafe fn from_marker_pair(
        buffer: OwnerBufferHandle,
        start: rune_editor::MarkerId,
        end: rune_editor::MarkerId,
        liveness: rune_editor::BufferLiveness,
        cx: &Context,
    ) -> Result<Self> {
        ensure!(
            buffer.belongs_to(cx) && liveness.is_live(),
            "Invalid Overlay marker owner/liveness"
        );
        Ok(Self {
            buffer,
            start,
            end,
            liveness,
            owner_liveness: cx.owner_liveness(),
            _prepared: (),
        })
    }
    fn is_live(&self) -> bool {
        self.liveness.is_live() && self.owner_liveness.is_live()
    }
}
pub(crate) struct OverlayInner {
    id: u64,
    owner: u64,
    owner_liveness: OwnerLiveness,
    front: bool,
    rear: bool,
    plist: ObjCell,
    attachment: RefCell<Option<OverlayAttachment>>,
}
pub(crate) struct LispOverlay(pub(in crate::core) GcHeap<OverlayInner>);
impl LispOverlay {
    pub(crate) fn new<'a>(cx: &'a Context<'_>, front: bool, rear: bool) -> Gc<&'a Self> {
        let id = NEXT_OVERLAY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("Overlay identities exhausted");
        // SAFETY: NIL is global; this fresh mutable overlay is immediately
        // allocated in its uniquely branded owning Context arena.
        let plist = unsafe { ObjCell::new(NIL) };
        Self(GcHeap::new(
            OverlayInner {
                id,
                owner: cx.domain_id(),
                owner_liveness: cx.owner_liveness(),
                front,
                rear,
                plist,
                attachment: RefCell::new(None),
            },
            false,
        ))
        .into_obj(&cx.block)
    }
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }
    pub(crate) fn front_advance(&self) -> bool {
        self.0.front
    }
    pub(crate) fn rear_advance(&self) -> bool {
        self.0.rear
    }
    pub(crate) fn validate(&self, cx: &Context) -> Result<()> {
        ensure!(
            self.0.owner == cx.domain_id() && self.0.owner_liveness.is_live(),
            "Foreign or retired Overlay owner"
        );
        ensure!(cx.block.belongs_to_owner(self.0.owner), "Overlay arena owner mismatch");
        Ok(())
    }
    pub(crate) fn plist<'a>(&'a self, cx: &'a Context) -> Result<Object<'a>> {
        self.validate(cx)?;
        Ok(self.0.plist.get())
    }
    /// SAFETY: value's entire reachable graph belongs to cx or audited globals,
    /// and remains rooted until this immediate callback-free publication.
    pub(crate) unsafe fn set_plist(&self, value: Object, cx: &Context) -> Result<()> {
        self.validate(cx)?;
        // SAFETY: overlays are mutable owner-only allocations, never global.
        unsafe {
            self.0.plist.as_mut().set(value);
        }
        Ok(())
    }
    pub(crate) fn attachment(&self, cx: &Context) -> Result<Option<OverlayAttachment>> {
        self.validate(cx)?;
        Ok(self.0.attachment.borrow().clone().filter(OverlayAttachment::is_live))
    }
    /// Caller prepares the canonical root and authenticated marker pair first;
    /// no evaluation/collection may intervene before this native-only commit.
    pub(crate) fn set_attachment(
        &self,
        attachment: Option<OverlayAttachment>,
        cx: &Context,
    ) -> Result<()> {
        self.validate(cx)?;
        if let Some(att) = &attachment {
            ensure!(
                att.buffer.belongs_to(cx) && att.is_live(),
                "Invalid Overlay attachment owner/liveness"
            );
        }
        let mut registry = cx.overlay_registry.try_borrow_mut()?;
        if let Some(att) = &attachment {
            let record = registry
                .roots
                .get_mut(&self.id())
                .ok_or_else(|| anyhow::anyhow!("Root Overlay before attaching markers"))?;
            record.attachment = Some(att.clone());
        } else if let Some(record) = registry.roots.get_mut(&self.id()) {
            record.attachment = None;
        }
        *self.0.attachment.borrow_mut() = attachment;
        Ok(())
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            AllocState::Forwarded(p) => Some(p),
            AllocState::Unmoved => None,
            AllocState::Global => panic!("global Overlay violates transfer invariant"),
        }
    }
}
impl Trace for OverlayInner {
    fn trace(&self, state: &mut GcState) {
        self.plist.trace(state);
    }
}
impl<'new> CloneIn<'new, &'new Self> for LispOverlay {
    fn clone_in<const C: bool>(&self, target: &'new Block<C>) -> Result<Gc<&'new Self>> {
        ensure!(
            target.belongs_to_owner(self.0.owner) && self.0.owner_liveness.is_live(),
            "Overlay transfer across heap domains is unsupported"
        );
        // SAFETY: only Context::with_domain stamps an arena. The private
        // constructor allocates this Overlay in exactly that owning arena;
        // all other destinations reject above. Allocation lives for target's
        // full borrow, including same-arena aliases; no native copy is made.
        Ok(unsafe { self.with_lifetime().tag() })
    }
}
impl PartialEq for LispOverlay {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}
impl Eq for LispOverlay {}
impl fmt::Debug for LispOverlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<overlay:{}>", self.id())
    }
}
impl fmt::Display for LispOverlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
crate::derive_GcMoveable!(LispOverlay);
struct OverlayRoot {
    value: Arc<HeapDomainValue>,
    attachment: Option<OverlayAttachment>,
}
#[derive(Default)]
pub(crate) struct OverlayRegistry {
    roots: HashMap<u64, OverlayRoot>,
}
impl OverlayRegistry {
    pub(in crate::core) fn prune_dead(&mut self) {
        self.roots
            .retain(|_, record| record.attachment.as_ref().is_none_or(OverlayAttachment::is_live));
    }
    pub(in crate::core) fn shutdown(&mut self) {
        self.roots.clear();
    }
}
impl Context<'_> {
    /// Allocate/register before the marker pair transaction, never afterward.
    pub(crate) fn root_overlay(&self, overlay: &LispOverlay) -> Result<()> {
        overlay.validate(self)?;
        if self.overlay_registry.borrow().roots.contains_key(&overlay.id()) {
            return Ok(());
        }
        // SAFETY: validated private owner allocation; properties are already
        // traced owner edges and no callback or collection occurs here.
        let value = Arc::new(unsafe { HeapDomainValue::new(overlay.into(), self) });
        let mut registry = self.overlay_registry.try_borrow_mut()?;
        registry.roots.try_reserve(1)?;
        registry
            .roots
            .insert(overlay.id(), OverlayRoot { value, attachment: overlay.attachment(self)? });
        Ok(())
    }
    pub(crate) fn unroot_overlay(&self, id: u64) -> Result<()> {
        self.overlay_registry.try_borrow_mut()?.roots.remove(&id);
        Ok(())
    }
    pub(crate) fn attached_overlays(
        &self,
        buffer: OwnerBufferHandle,
    ) -> Result<Vec<Arc<HeapDomainValue>>> {
        ensure!(buffer.belongs_to(self), "Foreign Overlay buffer owner");
        let registry = self.overlay_registry.try_borrow()?;
        let mut roots = Vec::new();
        roots.try_reserve(registry.roots.len())?;
        for record in registry.roots.values() {
            if record
                .attachment
                .as_ref()
                .is_some_and(|att| att.buffer == buffer && att.is_live())
            {
                roots.push(record.value.clone());
            }
        }
        Ok(roots)
    }
}

#[cfg(test)]
thread_local! { static DROPPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
impl Drop for OverlayInner {
    fn drop(&mut self) {
        DROPPED.with(|n| n.set(n.get() + 1));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::object::ObjectType;
    use crate::core::{
        env::{INTERNED_SYMBOLS, sym},
        gc::RootSet,
        object::make_channel_pair,
    };
    use rune_core::macros::root;
    #[test]
    fn detached_self_cycle_moves_when_rooted_and_collects_after_root_drop() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let before = DROPPED.with(std::cell::Cell::get);
        {
            let overlay = LispOverlay::new(cx, true, false);
            let id = overlay.id();
            // SAFETY: this self-edge belongs to this fresh owner arena.
            unsafe {
                overlay.set_plist(overlay.into(), cx).unwrap();
            }
            root!(overlay, cx);
            for _ in 0..5 {
                let old = Object::from(overlay.bind(cx)).into_raw().addr();
                cx.garbage_collect(true);
                assert_ne!(Object::from(overlay.bind(cx)).into_raw().addr(), old);
                assert_eq!(overlay.bind(cx).id(), id);
                assert!(overlay.bind(cx).plist(cx).unwrap().ptr_eq(Object::from(overlay.bind(cx))));
            }
            assert_eq!(DROPPED.with(std::cell::Cell::get), before);
        }
        cx.garbage_collect(true);
        assert_eq!(DROPPED.with(std::cell::Cell::get), before + 1);
    }
    #[test]
    fn same_arena_copy_preserves_identity_and_foreign_global_channel_copy_errors() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        let overlay = LispOverlay::new(cx, false, true);
        let alias = overlay.clone_in(&cx.block).unwrap();
        assert!(Object::from(alias).ptr_eq(Object::from(overlay)));
        {
            let symbols = INTERNED_SYMBOLS.lock().unwrap();
            assert!(overlay.clone_in(symbols.global_block()).is_err());
        }
        let (sender, receiver) = make_channel_pair(1);
        assert!(matches!(
            sender.try_send(overlay.into()),
            Err(crate::channel_manager::SendError::CopyFailed)
        ));
        assert!(matches!(receiver.try_recv(cx), Err(crate::channel_manager::RecvError::Empty)));
        sender.close();
        receiver.close();
    }
    #[test]
    fn attached_root_survives_gc_then_killed_buffer_prunes_without_editor_borrow() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let gateway = cx.editor_gateway().unwrap();
        let buffer = gateway.create("overlay-root-test", "abc", cx).unwrap();
        let before = DROPPED.with(std::cell::Cell::get);
        {
            let overlay = LispOverlay::new(cx, false, false);
            unsafe {
                overlay.set_plist(cx.add("only-overlay-property"), cx).unwrap();
            }
            cx.root_overlay(&overlay).unwrap();
            let (start, end) = gateway
                .register_marker_pair(
                    buffer,
                    0,
                    rune_editor::Bias::Left,
                    2,
                    rune_editor::Bias::Right,
                    cx,
                )
                .unwrap();
            overlay
                .set_attachment(
                    Some(unsafe {
                        OverlayAttachment::from_marker_pair(
                            buffer,
                            start,
                            end,
                            gateway.buffer_liveness(buffer, cx).unwrap(),
                            cx,
                        )
                        .unwrap()
                    }),
                    cx,
                )
                .unwrap();
        }
        cx.garbage_collect(true);
        {
            let values = cx.attached_overlays(buffer).unwrap();
            assert_eq!(values.len(), 1);
            let value: Gc<&LispOverlay> = values[0].bind(cx).unwrap().try_into().unwrap();
            assert_eq!(
                <&str>::try_from(value.plist(cx).unwrap()).unwrap(),
                "only-overlay-property"
            );
        }
        gateway.kill(buffer, true, cx).unwrap();
        cx.garbage_collect(true);
        assert!(cx.attached_overlays(buffer).unwrap().is_empty());
        assert_eq!(DROPPED.with(std::cell::Cell::get), before + 1);
    }
    #[test]
    fn intrinsic_overlay_error_rolls_back_prior_endpoint_copy_at_queue_and_global_hash() {
        use crate::channel_manager::{ChannelManager, RecvError, SendError};
        use crate::core::object::{HashTable, LispHashTable, make_channel_pair_for_test};
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        for global in [false, true] {
            let manager = ChannelManager::isolated_for_test();
            let (id, payload, observer) = make_channel_pair_for_test(manager.clone(), 1);
            let payload = cx.add(payload);
            let overlay = LispOverlay::new(cx, false, false);
            let graph = cx.add(vec![payload, overlay.into()]);
            if global {
                let table: Gc<&'static LispHashTable> = {
                    let map = INTERNED_SYMBOLS.lock().unwrap();
                    // SAFETY: empty native table in permanent global arena.
                    unsafe { HashTable::default().into_obj(map.global_block()).with_lifetime() }
                };
                assert!(table.untag().insert(42.into(), graph).is_err());
                assert!(table.untag().get(42.into()).is_none());
            } else {
                let (_, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
                assert_eq!(sender.try_send(graph), Err(SendError::CopyFailed));
                assert!(matches!(receiver.try_recv(cx), Err(RecvError::Empty)));
                sender.close();
                receiver.close();
            }
            let ObjectType::ChannelSender(payload) = payload.untag() else { unreachable!() };
            payload.close();
            assert_eq!(manager.counts_for_test(id), Some((0, 1)));
            assert!(matches!(observer.try_recv(cx), Err(RecvError::Closed)));
            observer.close();
        }
    }
    #[test]
    fn owner_retirement_invalidates_attachment_with_retained_gateway_and_snapshot() {
        let (gateway, buffer_liveness, owner_liveness, retained) = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let gateway = cx.editor_gateway().unwrap();
            let buffer = gateway.create("overlay-retired", "abc", &cx).unwrap();
            let overlay = LispOverlay::new(&cx, false, false);
            cx.root_overlay(&overlay).unwrap();
            let (start, end) = gateway
                .register_marker_pair(
                    buffer,
                    0,
                    rune_editor::Bias::Left,
                    1,
                    rune_editor::Bias::Right,
                    &cx,
                )
                .unwrap();
            let live = gateway.buffer_liveness(buffer, &cx).unwrap();
            // SAFETY: exact authenticated pair and token for this handle.
            let attachment = unsafe {
                OverlayAttachment::from_marker_pair(buffer, start, end, live.clone(), &cx).unwrap()
            };
            overlay.set_attachment(Some(attachment), &cx).unwrap();
            let retained = cx.attached_overlays(buffer).unwrap().pop().unwrap();
            (gateway, live, cx.owner_liveness(), retained)
        };
        assert!(buffer_liveness.is_live());
        assert!(!owner_liveness.is_live());
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert!(retained.bind(&cx).is_err());
        drop(gateway);
        assert!(!buffer_liveness.is_live());
    }

    #[test]
    fn prepared_root_abort_does_not_self_root_detached_property_cycles() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let before = DROPPED.with(std::cell::Cell::get);
        let id = {
            let overlay = LispOverlay::new(cx, true, true);
            unsafe {
                overlay.set_plist(overlay.into(), cx).unwrap();
            }
            cx.root_overlay(&overlay).unwrap();
            overlay.id()
        };
        cx.garbage_collect(true);
        assert_eq!(DROPPED.with(std::cell::Cell::get), before);
        cx.unroot_overlay(id).unwrap();
        cx.garbage_collect(true);
        assert_eq!(DROPPED.with(std::cell::Cell::get), before + 1);
    }
}
