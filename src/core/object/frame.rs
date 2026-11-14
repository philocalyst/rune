//! Owner-local logical frame identity. Headless frames are terminal, not OS windows.
use super::{CloneIn, Gc, HeapDomainValue, IntoObject};
use crate::core::gc::{AllocState, Block, Context, GcHeap, GcState, Trace};
use anyhow::{Result, ensure};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
static NEXT_FRAME: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameBackend {
    Terminal,
}
struct FrameIdentity {
    id: u64,
    owner: u64,
    live: AtomicBool,
}
pub(crate) struct FrameInner {
    identity: Arc<FrameIdentity>,
}
pub(crate) struct LispFrame(pub(in crate::core) GcHeap<FrameInner>);
impl LispFrame {
    pub(crate) fn id(&self) -> u64 {
        self.0.identity.id
    }
    pub(crate) fn backend(&self) -> FrameBackend {
        FrameBackend::Terminal
    }
    pub(crate) fn validate(&self, cx: &Context) -> Result<()> {
        ensure!(self.0.identity.owner == cx.domain_id(), "Frame belongs to another heap owner");
        ensure!(self.0.identity.live.load(Ordering::Acquire), "Frame is no longer live");
        Ok(())
    }
    pub(crate) fn delete(&self, cx: &Context) -> Result<()> {
        self.validate(cx)?;
        self.0.identity.live.store(false, Ordering::Release);
        Ok(())
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            AllocState::Forwarded(p) => Some(p),
            AllocState::Unmoved => None,
            AllocState::Global => panic!("global frame in local finalizers"),
        }
    }
}
impl Trace for FrameInner {
    fn trace(&self, _: &mut GcState) {}
}
impl<'new> CloneIn<'new, &'new Self> for LispFrame {
    fn clone_in<const C: bool>(&self, block: &'new Block<C>) -> anyhow::Result<Gc<&'new Self>> {
        Ok(Self(GcHeap::new(FrameInner { identity: self.0.identity.clone() }, C)).into_obj(block))
    }
}
impl PartialEq for LispFrame {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}
impl Eq for LispFrame {}
impl fmt::Debug for LispFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<terminal-frame:{}>", self.id())
    }
}
impl fmt::Display for LispFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
crate::derive_GcMoveable!(LispFrame);
#[derive(Default)]
pub(crate) struct FrameRegistry {
    selected: Option<Arc<HeapDomainValue>>,
    identity: Option<Arc<FrameIdentity>>,
}
impl FrameRegistry {
    pub(in crate::core) fn shutdown(&mut self) {
        if let Some(identity) = self.identity.take() {
            identity.live.store(false, Ordering::Release);
        }
        self.selected = None;
    }
}
impl Context<'_> {
    pub(crate) fn selected_frame(&self) -> Result<Gc<&LispFrame>> {
        let existing = self.frame_registry.borrow().selected.clone();
        if let Some(root) = existing {
            let frame: Gc<&LispFrame> = root.bind(self)?.try_into()?;
            frame.validate(self)?;
            return Ok(frame);
        }
        let id = NEXT_FRAME
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("Frame identities exhausted");
        let identity =
            Arc::new(FrameIdentity { id, owner: self.domain_id(), live: AtomicBool::new(true) });
        let frame = LispFrame(GcHeap::new(FrameInner { identity: identity.clone() }, false))
            .into_obj(&self.block);
        // SAFETY: fresh owner-arena leaf contains native identity only.
        let root = Arc::new(unsafe { HeapDomainValue::new(frame.into(), self) });
        let mut registry = self.frame_registry.borrow_mut();
        registry.identity = Some(identity);
        registry.selected = Some(root);
        Ok(frame)
    }
    pub(crate) fn live_frames(&self) -> Result<Vec<Gc<&LispFrame>>> {
        let root = self.frame_registry.borrow().selected.clone();
        if let Some(root) = root {
            let frame: Gc<&LispFrame> = root.bind(self)?.try_into()?;
            if frame.validate(self).is_err() {
                return Ok(Vec::new());
            }
            return Ok(vec![frame]);
        }
        Ok(vec![self.selected_frame()?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn canonical_terminal_frame_survives_moving_gc_and_delete() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let frame = cx.selected_frame().unwrap();
        let id = frame.id();
        root!(frame, cx);
        for _ in 0..5 {
            cx.garbage_collect(true);
            assert_eq!(cx.selected_frame().unwrap().id(), id);
            assert_eq!(frame.bind(cx).id(), id);
            assert_eq!(cx.live_frames().unwrap().len(), 1);
            assert_eq!(frame.bind(cx).backend(), FrameBackend::Terminal);
        }
        frame.bind(cx).delete(cx).unwrap();
        assert!(cx.live_frames().unwrap().is_empty());
        assert!(cx.selected_frame().is_err());
        assert!(frame.bind(cx).validate(cx).is_err());
    }
    #[test]
    fn foreign_copy_cannot_become_owner_frame_and_teardown_tombstones_identity() {
        let (copy, identity) = {
            let source_roots = RootSet::default();
            let source = Context::new(&source_roots);
            let frame = source.selected_frame().unwrap();
            let identity = frame.0.identity.clone();
            let copy = LispFrame(GcHeap::new(FrameInner { identity: identity.clone() }, false));
            (copy, identity)
        };
        let target_roots = RootSet::default();
        let target = &mut Context::new(&target_roots);
        let copy = copy.into_obj(&target.block);
        assert!(!identity.live.load(Ordering::Acquire));
        root!(copy, target);
        target.garbage_collect(true);
        assert!(copy.bind(target).validate(target).is_err());
        assert_ne!(target.selected_frame().unwrap().id(), copy.bind(target).id());
    }
}
