//! Heap-domain ownership for Lisp values stored in globally named buffers.
//!
//! Text locks and Lisp metadata locks are deliberately separate. A Context's
//! weak registry traces every live owned metadata store, including non-current
//! buffers, without acquiring an already-held text-buffer mutex.
//! When an owner ends its metadata is permanently tombstoned. Global buffer names
//! and text remain discoverable, but a new Context must create a new buffer rather
//! than silently adopting a retired store. Property access and text edits reject it.
use crate::{
    core::gc::{Context, GcState, Trace},
    intervals::IntervalTree,
};
use anyhow::{Result, bail};
use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard, Weak},
};

#[derive(Clone)]
pub(crate) struct BufferProperties(Arc<BufferPropertyRoot>);

impl std::fmt::Debug for BufferProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not lock or format moving Lisp values here. Debug may run while
        // the caller holds either the text or metadata mutex, and property
        // values may refer back to that very buffer.
        f.write_str("BufferProperties(..)")
    }
}

pub(crate) struct BufferPropertyRoot {
    state: Mutex<PropertyState>,
}

struct PropertyState {
    owner: Option<u64>,
    closed: bool,
    tree: IntervalTree<'static>,
}

// SAFETY: This private state is never exposed without the mutex and an owning
// Context check. Collection takes that same mutex; retirement clears all slots
// before the owner heap dies. The raw IntervalTree itself must remain !Send.
unsafe impl Send for PropertyState {}

impl BufferProperties {
    pub(crate) fn new() -> Self {
        Self(Arc::new(BufferPropertyRoot {
            state: Mutex::new(PropertyState {
                owner: None,
                closed: false,
                tree: IntervalTree::new(),
            }),
        }))
    }

    /// The returned guard borrows the owning Context. Safe Rust cannot collect
    /// that Context while this guard is alive. No Lisp callback may run under it.
    pub(crate) fn lock<'a>(&'a self, cx: &'a Context) -> Result<BufferPropertyGuard<'a>> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Buffer property lock poisoned"))?;
        if state.closed {
            bail!("Buffer property owner context has ended");
        }
        match state.owner {
            Some(owner) if owner != cx.domain_id() => {
                bail!("Buffer properties belong to another heap domain")
            }
            Some(_) => {}
            None => {
                // The unclaimed tree is empty: no foreign pointers can enter
                // before an owner is registered.
                cx.register_buffer_properties(Arc::downgrade(&self.0));
                state.owner = Some(cx.domain_id());
            }
        }
        Ok(BufferPropertyGuard { state, context: PhantomData })
    }
}

/// One independently retained Lisp value owned by a heap domain. This shares
/// the collector's domain registry, but has no buffer naming, edits, or kill
/// lifecycle. Owner teardown retires it before releasing the heap.
#[derive(Debug)]
pub(crate) struct HeapDomainValue(BufferProperties);

impl HeapDomainValue {
    /// SAFETY: Every reachable local allocation in value must belong to cx;
    /// globally shared allocations must satisfy their audited sharing contract.
    /// Registration checks the destination owner, not payload provenance.
    pub(crate) unsafe fn new(value: super::Object, cx: &Context) -> Self {
        let slots = BufferProperties::new();
        slots.lock(cx).unwrap().set_properties(1, 2, value);
        Self(slots)
    }

    pub(crate) fn bind<'ob>(&self, cx: &'ob Context) -> Result<super::Object<'ob>> {
        let slots = self.0.lock(cx)?;
        // IntervalTree canonicalizes an explicit NIL slot away. That is still
        // a valid retained value, not a missing/corrupt root.
        let value = slots.find(1).map(|node| *node.val).unwrap_or(super::NIL);
        // Owner validation establishes cx's lifetime, independent of the slot
        // mutex guard. No mutable Context collection can overlap that borrow.
        Ok(unsafe { crate::core::object::WithLifetime::with_lifetime(value) })
    }
}

pub(crate) struct BufferPropertyGuard<'a> {
    state: MutexGuard<'a, PropertyState>,
    context: PhantomData<&'a ()>,
}

impl<'a> Deref for BufferPropertyGuard<'a> {
    type Target = IntervalTree<'a>;
    fn deref(&self) -> &Self::Target {
        // Ownership was checked while acquiring the guard; the Context registry
        // keeps every slot live and updates it during collection. Narrow only.
        unsafe { &*(&self.state.tree as *const IntervalTree<'static>).cast::<IntervalTree<'a>>() }
    }
}
impl DerefMut for BufferPropertyGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *(&mut self.state.tree as *mut IntervalTree<'static>).cast() }
    }
}

impl BufferPropertyRoot {
    pub(crate) fn trace_owned(&self, domain: u64, state: &mut GcState) {
        let properties = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(properties.owner, Some(domain), "foreign buffer registered with collector");
        if !properties.closed {
            properties.tree.trace(state);
        }
    }
    pub(crate) fn close_owned(&self, domain: u64) {
        let mut properties = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(properties.owner, Some(domain));
        properties.tree = IntervalTree::new();
        properties.closed = true;
    }
}

pub(crate) type WeakBufferPropertyRoot = Weak<BufferPropertyRoot>;

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::core::{
        cons::Cons,
        env::{Env, intern},
        gc::RootSet,
        object::{NIL, Object},
    };
    use rune_core::macros::root;

    #[test]
    fn generic_domain_value_retains_nil_without_a_tree_node() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let value = unsafe { HeapDomainValue::new(NIL, cx) };
        cx.garbage_collect(true);
        assert!(value.bind(cx).unwrap().is_nil());
    }

    #[test]
    fn debug_does_not_reenter_held_metadata_mutex() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let properties = BufferProperties::new();
        let _guard = properties.lock(&cx).unwrap();
        assert_eq!(format!("{properties:?}"), "BufferProperties(..)");
    }

    #[test]
    fn noncurrent_and_explicit_buffer_properties_survive_repeated_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        let first = cx.add(
            crate::core::object::LispBuffer::for_handle(env.current_buffer_handle(), cx).unwrap(),
        );
        root!(first, cx);
        let other = crate::buffer::get_buffer_create(
            cx.add(format!("owner-domain-other-{}", cx.domain_id())),
            None,
            cx,
        )
        .unwrap();
        root!(other, cx);
        let gateway = env.editor_gateway();
        for handle in [env.current_buffer_handle(), other.bind(cx).buffer().unwrap().handle()] {
            let read = gateway.read(handle, cx).unwrap();
            gateway.splice(handle, read.epoch, 0..0, "abc", cx).unwrap();
        }
        let key = cx.add(intern("domain-property", cx));
        let value = cx.add("shared allocated property value");
        crate::textprops::put_text_property(1, 3, key, value, Some(first.bind(cx)), env, cx)
            .unwrap();
        // Explicitly targeting a non-current buffer must register it too.
        crate::textprops::put_text_property(1, 3, key, value, Some(other.bind(cx)), env, cx)
            .unwrap();
        env.set_buffer(other.bind(cx).buffer().unwrap(), cx).unwrap();
        for iteration in 0..30 {
            for garbage in 0..50 {
                let _ = cx.add(format!("garbage {iteration} {garbage}"));
            }
            cx.garbage_collect(true);
            let key = cx.add(intern("domain-property", cx));
            let left =
                crate::textprops::get_text_property(1, key, Some(first.bind(cx)), env, cx).unwrap();
            let right =
                crate::textprops::get_text_property(1, key, Some(other.bind(cx)), env, cx).unwrap();
            assert!(left.ptr_eq(right), "shared value identity changed");
            assert_eq!(left.to_string(), "\"shared allocated property value\"");
        }
    }

    #[test]
    fn context_teardown_tombstones_metadata_before_heap_release() {
        let properties = BufferProperties::new();
        {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            let plist = Cons::new(cx.add("key"), Cons::new(cx.add("allocated"), NIL, cx), cx);
            properties.lock(cx).unwrap().set_properties(1, 2, plist.into());
            cx.garbage_collect(true);
        }
        {
            let state = properties.0.state.lock().unwrap();
            assert!(state.closed);
            assert!(state.tree.find(1).is_none());
        }
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert!(properties.lock(&cx).is_err());
    }

    #[test]
    fn foreign_domain_access_is_rejected_during_owner_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let properties = BufferProperties::new();
        let plist = Cons::new(cx.add("key"), Cons::new(cx.add("owner value"), NIL, cx), cx);
        properties.lock(cx).unwrap().set_properties(1, 2, plist.into());
        let foreign = properties.clone();
        let thread = std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            for _ in 0..100 {
                assert!(foreign.lock(&cx).is_err());
            }
        });
        for _ in 0..30 {
            cx.garbage_collect(true);
        }
        thread.join().unwrap();
        assert!(properties.lock(cx).unwrap().find(1).is_some());
    }

    #[test]
    fn foreign_text_edits_are_rejected_before_mutation() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let gateway = cx.editor_gateway().unwrap();
        let handle = gateway.create("foreign-owned-edit", "owner", cx).unwrap();
        let thread = std::thread::spawn(move || {
            let roots = RootSet::default();
            let foreign = Context::new(&roots);
            let other = foreign.editor_gateway().unwrap();
            assert!(other.read(handle, &foreign).is_err());
            assert!(other.splice(handle, 0, 0..3, "foreign", &foreign).is_err());
        });
        for _ in 0..10 {
            cx.garbage_collect(true);
        }
        thread.join().unwrap();
        assert_eq!(gateway.read(handle, cx).unwrap().document.text().to_string(), "owner");
    }

    #[test]
    fn retained_metadata_survives_killed_buffer_and_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let gateway = cx.editor_gateway().unwrap();
        let handle = gateway.create("retained-killed-domain", "abc", cx).unwrap();
        // SAFETY: fresh scalar value allocated in this exact owner Context.
        let property = unsafe { gateway.intern_property(cx.add("retained value"), cx) }.unwrap();
        let id = property.id();
        let read = gateway.read(handle, cx).unwrap();
        let prepared = gateway
            .prepare_properties(&read, rune_text::CharPos(0)..rune_text::CharPos(3), &property, cx)
            .unwrap();
        gateway.publish_properties(prepared, cx).unwrap();
        drop(property);
        drop(read);
        let retained = gateway.read(handle, cx).unwrap();
        gateway.kill(handle, true, cx).unwrap();
        for _ in 0..10 {
            cx.garbage_collect(true);
            assert_eq!(retained.document.resolve(id, cx).unwrap(), "retained value");
        }
        assert!(gateway.read(handle, cx).is_err());
    }

    #[test]
    fn retired_shell_rejects_edits_and_same_name_new_owner_is_independent() {
        let (old, retained) = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let gateway = cx.editor_gateway().unwrap();
            let old = gateway.create("retired-domain-name", "original", &cx).unwrap();
            (old, gateway.read(old, &cx).unwrap())
        };
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let gateway = cx.editor_gateway().unwrap();
        assert!(gateway.read(old, &cx).is_err());
        assert!(gateway.splice(old, retained.epoch, 0..3, "foreign", &cx).is_err());
        let fresh = gateway.create("retired-domain-name", "new", &cx).unwrap();
        assert_ne!(old, fresh);
        assert_eq!(gateway.read(fresh, &cx).unwrap().document.text().to_string(), "new");
        assert_eq!(retained.document.text().to_string(), "original");
    }

    #[test]
    fn weak_registry_does_not_keep_killed_stores_alive() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let properties = BufferProperties::new();
        let weak = Arc::downgrade(&properties.0);
        properties.lock(cx).unwrap().set_properties(1, 2, Object::from(7_i64));
        drop(properties);
        assert!(weak.upgrade().is_none());
        cx.garbage_collect(true);
    }
}
