//! Opaque retained property IDs for runtime-owned text/undo snapshots.
//!
//! Pure text carries u32 IDs only. Handles retain their Lisp values and leases
//! retain exactly the IDs needed by a snapshot plus its insertion policy. Neither
//! is an authorization claim or immutable rendering: Lisp values may mutate, so
//! resolved render styles need an independent epoch. Only resolve touches Lisp.
use super::{HeapDomainValue, NIL, Object};
use crate::core::gc::Context;
use anyhow::{Result, bail, ensure};
use std::{cell::Cell, collections::BTreeMap, marker::PhantomData, rc::Rc, sync::Arc};

#[derive(Debug)]
struct RegistryIdentity {
    owner: u64,
}

/// Owner-thread allocator. Registry identity is private and never a numeric ID
/// supplied by an editor. Separate registries in one Context cannot mix handles.
#[derive(Debug)]
pub(crate) struct PropertyRegistry {
    identity: Arc<RegistryIdentity>,
    next_id: Cell<u32>,
    owner_thread: PhantomData<Rc<()>>,
}

/// An ID alone is not a root. Keep this handle until a prepared lease retains it.
#[derive(Clone, Debug)]
pub(crate) struct PropertySetHandle {
    identity: Arc<RegistryIdentity>,
    id: u32,
    value: Option<Arc<HeapDomainValue>>,
}

/// Sendable retention-only token. It contains no Context, exposes no raw slots,
/// and can be cloned/dropped by undo or publication workers. Last drop only
/// releases slot storage; the Context's weak root registry handles reclamation.
#[derive(Clone, Debug)]
pub(crate) struct RuntimePropertyLease {
    identity: Arc<RegistryIdentity>,
    retained: Arc<BTreeMap<u32, Arc<HeapDomainValue>>>,
    insertion_id: u32,
}

impl PropertyRegistry {
    pub(crate) fn new(cx: &Context) -> Self {
        Self {
            identity: Arc::new(RegistryIdentity { owner: cx.domain_id() }),
            next_id: Cell::new(1),
            owner_thread: PhantomData,
        }
    }

    fn check_owner(&self, cx: &Context) -> Result<()> {
        ensure!(self.identity.owner == cx.domain_id(), "Foreign property heap domain");
        Ok(())
    }

    pub(crate) fn empty(&self, cx: &Context) -> Result<PropertySetHandle> {
        self.check_owner(cx)?;
        Ok(PropertySetHandle { identity: self.identity.clone(), id: 0, value: None })
    }

    /// No content deduplication: property graphs may be cyclic or mutable.
    /// IDs never recycle, and exhaustion fails before creating a root or ID.
    /// SAFETY: The caller must establish that every reachable local allocation
    /// in value belongs to cx (or is audited globally shared storage). These
    /// checks authenticate the registry/owner, not arbitrary Object provenance.
    pub(crate) unsafe fn intern(&self, value: Object, cx: &Context) -> Result<PropertySetHandle> {
        self.check_owner(cx)?;
        if value.is_nil() {
            return self.empty(cx);
        }
        let id = self.next_id.get();
        let next = id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Property ID space exhausted"))?;
        let value = Arc::new(unsafe { HeapDomainValue::new(value, cx) });
        self.next_id.set(next);
        Ok(PropertySetHandle { identity: self.identity.clone(), id, value: Some(value) })
    }

    /// The runtime adapter enumerates every ID in the snapshot before calling
    /// this. All source leases/new handles must belong to this exact registry.
    /// A raw pure Snapshot must never be attached to a different registry merely
    /// because its numeric IDs happen to exist there.
    pub(crate) fn prepare_lease<'a>(
        &self,
        required_ids: impl IntoIterator<Item = u32>,
        insertion: &PropertySetHandle,
        sources: impl IntoIterator<Item = &'a RuntimePropertyLease>,
        handles: impl IntoIterator<Item = &'a PropertySetHandle>,
        cx: &Context,
    ) -> Result<RuntimePropertyLease> {
        self.check_owner(cx)?;
        prepare(self.identity.clone(), required_ids, insertion, sources, handles)
    }
}

impl PropertySetHandle {
    pub(crate) fn id(&self) -> u32 {
        self.id
    }
}

impl RuntimePropertyLease {
    pub(crate) fn insertion_id(&self) -> u32 {
        self.insertion_id
    }

    /// Metadata-only restriction for pure Document::replace / undo. No Lisp
    /// access, allocation in a Context, or owner-thread release callback occurs.
    /// Existing retained IDs and a same-registry minted insertion handle are
    /// the only candidates; an unknown or foreign ID fails before publication.
    pub(crate) fn retain_for_snapshot(
        &self,
        required_ids: impl IntoIterator<Item = u32>,
        insertion: &PropertySetHandle,
    ) -> Result<Self> {
        prepare(self.identity.clone(), required_ids, insertion, [self], std::iter::empty())
    }

    pub(crate) fn contains(&self, id: u32) -> bool {
        id == 0 || self.retained.contains_key(&id)
    }

    /// Resolve only on the owning heap. A retired owner's ID cannot match a
    /// new Context; retained root stores are also tombstoned before heap release.
    pub(crate) fn resolve<'ob>(&self, id: u32, cx: &'ob Context) -> Result<Object<'ob>> {
        ensure!(self.identity.owner == cx.domain_id(), "Foreign property heap domain");
        if id == 0 {
            return Ok(NIL);
        }
        self.retained
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("Property ID is not retained"))?
            .bind(cx)
    }
}

fn add_value(
    pool: &mut BTreeMap<u32, Arc<HeapDomainValue>>,
    id: u32,
    value: &Arc<HeapDomainValue>,
) -> Result<()> {
    if let Some(old) = pool.get(&id) {
        ensure!(Arc::ptr_eq(old, value), "Conflicting roots for one property ID");
    } else {
        pool.insert(id, value.clone());
    }
    Ok(())
}

fn add_handle(
    identity: &Arc<RegistryIdentity>,
    pool: &mut BTreeMap<u32, Arc<HeapDomainValue>>,
    handle: &PropertySetHandle,
) -> Result<()> {
    ensure!(Arc::ptr_eq(identity, &handle.identity), "Foreign property registry");
    match (handle.id, handle.value.as_ref()) {
        (0, None) => Ok(()),
        (0, Some(_)) | (_, None) => bail!("Invalid property root handle"),
        (id, Some(value)) => add_value(pool, id, value),
    }
}

fn prepare<'a>(
    identity: Arc<RegistryIdentity>,
    required_ids: impl IntoIterator<Item = u32>,
    insertion: &PropertySetHandle,
    sources: impl IntoIterator<Item = &'a RuntimePropertyLease>,
    handles: impl IntoIterator<Item = &'a PropertySetHandle>,
) -> Result<RuntimePropertyLease> {
    let mut pool = BTreeMap::new();
    add_handle(&identity, &mut pool, insertion)?;
    for source in sources {
        ensure!(Arc::ptr_eq(&identity, &source.identity), "Foreign property registry");
        for (&id, value) in source.retained.iter() {
            add_value(&mut pool, id, value)?;
        }
    }
    for handle in handles {
        add_handle(&identity, &mut pool, handle)?;
    }
    let mut retained = BTreeMap::new();
    for id in required_ids.into_iter().chain([insertion.id]) {
        if id != 0 {
            let value =
                pool.get(&id).ok_or_else(|| anyhow::anyhow!("Unrooted property ID {id}"))?;
            retained.insert(id, value.clone());
        }
    }
    Ok(RuntimePropertyLease { identity, retained: Arc::new(retained), insertion_id: insertion.id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;

    #[test]
    fn registry_identity_rejects_same_numeric_id_from_other_registry() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let a = PropertyRegistry::new(&cx);
        let b = PropertyRegistry::new(&cx);
        let left = unsafe { a.intern(cx.add("left"), &cx) }.unwrap();
        let right = unsafe { b.intern(cx.add("right"), &cx) }.unwrap();
        assert_eq!(left.id(), right.id());
        let empty = a.empty(&cx).unwrap();
        assert!(a.prepare_lease([left.id()], &empty, [], [&right], &cx).is_err());
        let foreign = b.prepare_lease([right.id()], &right, [], [&right], &cx).unwrap();
        assert!(a.prepare_lease([left.id()], &empty, [&foreign], [&left], &cx).is_err());
    }

    #[test]
    fn duplicate_handles_must_retain_exact_same_root() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let registry = PropertyRegistry::new(&cx);
        let handle = unsafe { registry.intern(cx.add("one"), &cx) }.unwrap();
        let lease = registry
            .prepare_lease([handle.id()], &handle, [], [&handle, &handle], &cx)
            .unwrap();
        assert_eq!(lease.resolve(handle.id(), &cx).unwrap(), "one");
        let conflict = PropertySetHandle {
            identity: handle.identity.clone(),
            id: handle.id,
            value: Some(Arc::new(unsafe { HeapDomainValue::new(cx.add("two"), &cx) })),
        };
        assert!(registry.prepare_lease([handle.id()], &handle, [], [&conflict], &cx).is_err());
    }

    #[test]
    fn unknown_ids_and_exhaustion_fail_before_publication() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let registry = PropertyRegistry::new(&cx);
        let empty = registry.empty(&cx).unwrap();
        assert!(registry.prepare_lease([99], &empty, [], [], &cx).is_err());
        registry.next_id.set(u32::MAX);
        assert!(unsafe { registry.intern(cx.add("overflow"), &cx) }.is_err());
        assert_eq!(registry.next_id.get(), u32::MAX);
    }

    #[test]
    fn undo_lease_alone_roots_values_across_gc_and_foreign_drop() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuntimePropertyLease>();
        assert_send_sync::<PropertySetHandle>();
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let registry = PropertyRegistry::new(cx);
        let empty = registry.empty(cx).unwrap();
        let handle = unsafe { registry.intern(cx.add("undo-only value"), cx) }.unwrap();
        let id = handle.id();
        let weak_value = Arc::downgrade(handle.value.as_ref().unwrap());
        let undo = registry.prepare_lease([id], &empty, [], [&handle], cx).unwrap();
        drop(handle);
        let current = undo.retain_for_snapshot([], &empty).unwrap();
        assert!(!current.contains(id));
        assert_eq!(current.insertion_id(), 0);
        let foreign = undo.clone();
        let thread = std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            assert!(foreign.contains(id));
            assert!(foreign.resolve(id, &cx).is_err());
            drop(foreign);
        });
        for _ in 0..10 {
            cx.garbage_collect(true);
            assert_eq!(undo.resolve(id, cx).unwrap(), "undo-only value");
        }
        thread.join().unwrap();
        assert!(weak_value.upgrade().is_some());
        std::thread::spawn(move || drop(undo)).join().unwrap();
        assert!(weak_value.upgrade().is_none(), "registry/current snapshot retained obsolete ID");
        cx.garbage_collect(true);
    }

    #[test]
    fn retained_lease_rejects_resolution_after_owner_teardown() {
        let (lease, id) = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let registry = PropertyRegistry::new(&cx);
            let handle = unsafe { registry.intern(cx.add("retired value"), &cx) }.unwrap();
            let id = handle.id();
            (registry.prepare_lease([id], &handle, [], [&handle], &cx).unwrap(), id)
        };
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert!(lease.contains(id));
        assert!(lease.resolve(id, &cx).is_err());
    }
}
