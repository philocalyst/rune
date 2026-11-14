use super::GcState;
use super::Trace;
use crate::core::object::GcString;
use crate::core::object::WeakBufferPropertyRoot;
use crate::core::object::{
    ByteFn, ChannelReceiver, ChannelSender, CharTable, FrameRegistry, LispBigInt, LispFrame,
    LispHashTable, LispOverlay, LispProcess, OverlayRegistry, ProcessRegistry,
};
use crate::core::object::{Gc, IntoObject, Object, UninternedSymbolMap, WithLifetime};
use bumpalo::collections::Vec as GcVec;
use std::cell::{Cell, OnceCell, RefCell};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Read-only native witness of owner lifetime. Contains no Lisp pointers and
/// grants no authority to access a heap; domain authentication is still required.
#[derive(Clone, Debug)]
pub(crate) struct OwnerLiveness(std::sync::Arc<AtomicBool>);
impl OwnerLiveness {
    pub(crate) fn is_live(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A global store of all gc roots. This struct should be passed to the [Context]
/// when it is created.
#[derive(Default, Debug)]
pub(crate) struct RootSet {
    pub(super) roots: RefCell<Vec<*const dyn Trace>>,
}

/// Thread-safe version of RootSet that uses Mutex instead of RefCell.
/// This is specifically designed for multi-threaded scenarios like ChannelManager
/// where roots need to be managed across thread boundaries.
///
/// Note: Send/Sync safety is guaranteed by HeapRoot, which ensures the raw pointers
/// only point to data it owns in HeapRoot::data.
#[derive(Debug)]
pub(crate) struct ThreadSafeRootSet {
    pub(super) roots: std::sync::Mutex<Vec<*const dyn Trace>>,
}

impl Default for ThreadSafeRootSet {
    fn default() -> Self {
        Self { roots: std::sync::Mutex::new(Vec::new()) }
    }
}

#[expect(dead_code)]
// These types are only stored here so they can be dropped
pub(in crate::core) enum DropStackElem {
    String(String),
    ByteString(Vec<u8>),
    Vec(Vec<Object<'static>>),
}

/// A block of allocations. This type should be owned by [Context] and not used
/// directly.
pub(crate) struct Block<const CONST: bool> {
    // Assigned only by the audited fresh manager-arena constructor. Ordinary
    // owner/global arenas are external relative to every channel manager.
    channel_manager: Option<crate::channel_manager::ChannelManagerIdentity>,
    owner_domain: Option<u64>,
    copy_allocations: RefCell<Option<Vec<CopyAllocation>>>,
    copy_walk: RefCell<CopyWalk>,
    #[cfg(test)]
    copy_failure_after: Cell<Option<usize>>,
    pub(in crate::core) objects: bumpalo::Bump,
    // Allocations that will be dropped when the objects are moved. At that time
    // the allocation will get copied into the GC heap. This let's us avoid an
    // extra copy of memory when a vector is first made an object. The
    // allocation can continue to live outside of the GC heap until we copy the
    // object.
    pub(in crate::core) drop_stack: RefCell<Vec<DropStackElem>>,
    // We don't yet have a hashmap that supports allocators, so we need to keep
    // track of the memory and free it only after the table is garbage
    // collected. Kind of a hack.
    pub(in crate::core) lisp_hashtables: RefCell<Vec<*const LispHashTable>>,
    // Char-tables also own native map/mutex resources requiring finalization.
    pub(in crate::core) lisp_chartables: RefCell<Vec<*const CharTable>>,
    pub(in crate::core) lisp_senders: RefCell<Vec<*const ChannelSender>>,
    pub(in crate::core) lisp_receivers: RefCell<Vec<*const ChannelReceiver>>,
    pub(in crate::core) lisp_overlays: RefCell<Vec<*const LispOverlay>>,
    pub(in crate::core) lisp_frames: RefCell<Vec<*const LispFrame>>,
    pub(in crate::core) lisp_processes: RefCell<Vec<*const LispProcess>>,
    pub(in crate::core) uninterned_symbol_map: UninternedSymbolMap,
}

unsafe impl<const C: bool> Send for Block<C> {}

/// Owned wrappers moved out of from-space, not deferred arena pointers. Declare
/// before collector guards so native callbacks run after their release, including
/// unwind AFTER completed collection. This does not make mid-GC panic recoverable.
#[derive(Default)]
pub(crate) struct DeferredEndpointFinalizers {
    senders: Vec<ChannelSender>,
    receivers: Vec<ChannelReceiver>,
    processes: Vec<LispProcess>,
    frames: Vec<LispFrame>,
    overlays: Vec<LispOverlay>,
    copy_tables: Vec<LispHashTable>,
    copy_chartables: Vec<CharTable>,
    copy_integers: Vec<LispBigInt>,
    copy_bytefns: Vec<ByteFn>,
    copy_storage: Vec<DropStackElem>,
}
#[derive(Default)]
struct CopyWalk {
    active: Vec<usize>,
    work: usize,
}
pub(crate) struct CopyVisit<'a, const C: bool> {
    block: &'a Block<C>,
    key: usize,
}
impl<const C: bool> Drop for CopyVisit<'_, C> {
    fn drop(&mut self) {
        let mut walk = self.block.copy_walk.borrow_mut();
        let popped = walk.active.pop();
        debug_assert_eq!(popped, Some(self.key));
        if walk.active.is_empty() {
            walk.work = 0;
        }
    }
}

/// Owning native resources allocated during an unpublished graph copy. These
/// are not roots and may only be consumed before any collection of the arena.
pub(in crate::core) enum CopyAllocation {
    Sender(*const ChannelSender),
    Receiver(*const ChannelReceiver),
    Process(*const LispProcess),
    Frame(*const LispFrame),
    Overlay(*const LispOverlay),
    Table(*const LispHashTable),
    CharTable(*const CharTable),
    Integer(*const LispBigInt),
    ByteFn(*const ByteFn),
}
#[cfg(test)]
struct CopyFailureScope<'a, const C: bool> {
    block: &'a Block<C>,
    previous: Option<usize>,
}
#[cfg(test)]
impl<const C: bool> Drop for CopyFailureScope<'_, C> {
    fn drop(&mut self) {
        self.block.copy_failure_after.set(self.previous);
    }
}
pub(crate) struct CopyTransaction<'a, const C: bool> {
    block: &'a Block<C>,
    deferred: &'a mut DeferredEndpointFinalizers,
    lengths: [usize; 8],
    committed: bool,
}
impl<const C: bool> CopyTransaction<'_, C> {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}
impl<const C: bool> Drop for CopyTransaction<'_, C> {
    fn drop(&mut self) {
        let allocations = self
            .block
            .copy_allocations
            .borrow_mut()
            .take()
            .expect("active copy transaction");
        if self.committed {
            return;
        }
        self.block.lisp_senders.borrow_mut().truncate(self.lengths[0]);
        self.block.lisp_receivers.borrow_mut().truncate(self.lengths[1]);
        self.block.lisp_processes.borrow_mut().truncate(self.lengths[2]);
        self.block.lisp_frames.borrow_mut().truncate(self.lengths[3]);
        self.block.lisp_overlays.borrow_mut().truncate(self.lengths[7]);
        self.block.lisp_hashtables.borrow_mut().truncate(self.lengths[4]);
        self.block.lisp_chartables.borrow_mut().truncate(self.lengths[5]);
        self.deferred
            .copy_storage
            .extend(self.block.drop_stack.borrow_mut().drain(self.lengths[6]..));
        // SAFETY: each recorded pointer is a distinct newly allocated owning
        // wrapper in this exclusive transaction, never published or collected.
        // Remove its finalizer registration before transferring ownership.
        for allocation in allocations {
            unsafe {
                match allocation {
                    CopyAllocation::Sender(p) => self.deferred.senders.push(std::ptr::read(p)),
                    CopyAllocation::Receiver(p) => self.deferred.receivers.push(std::ptr::read(p)),
                    CopyAllocation::Process(p) => self.deferred.processes.push(std::ptr::read(p)),
                    CopyAllocation::Frame(p) => self.deferred.frames.push(std::ptr::read(p)),
                    CopyAllocation::Overlay(p) => self.deferred.overlays.push(std::ptr::read(p)),
                    CopyAllocation::Table(p) => self.deferred.copy_tables.push(std::ptr::read(p)),
                    CopyAllocation::CharTable(p) => {
                        self.deferred.copy_chartables.push(std::ptr::read(p))
                    }
                    CopyAllocation::Integer(p) => {
                        self.deferred.copy_integers.push(std::ptr::read(p))
                    }
                    CopyAllocation::ByteFn(p) => self.deferred.copy_bytefns.push(std::ptr::read(p)),
                }
            }
        }
    }
}
fn collect_endpoint_finalizers(block: &Block<false>, deferred: &mut DeferredEndpointFinalizers) {
    block.lisp_senders.borrow_mut().retain_mut(|pointer| {
        if let Some(new) = unsafe { &**pointer }.forwarding_ptr() {
            *pointer = new.as_ptr().cast();
            true
        } else {
            deferred.senders.push(unsafe { std::ptr::read(*pointer) });
            false
        }
    });
    block.lisp_receivers.borrow_mut().retain_mut(|pointer| {
        if let Some(new) = unsafe { &**pointer }.forwarding_ptr() {
            *pointer = new.as_ptr().cast();
            true
        } else {
            deferred.receivers.push(unsafe { std::ptr::read(*pointer) });
            false
        }
    });
    block.lisp_processes.borrow_mut().retain_mut(|pointer| {
        if let Some(new) = unsafe { &**pointer }.forwarding_ptr() {
            *pointer = new.as_ptr().cast();
            true
        } else {
            deferred.processes.push(unsafe { std::ptr::read(*pointer) });
            false
        }
    });
    block.lisp_frames.borrow_mut().retain_mut(|pointer| {
        if let Some(new) = unsafe { &**pointer }.forwarding_ptr() {
            *pointer = new.as_ptr().cast();
            true
        } else {
            deferred.frames.push(unsafe { std::ptr::read(*pointer) });
            false
        }
    });
    block.lisp_overlays.borrow_mut().retain_mut(|pointer| {
        if let Some(new) = unsafe { &**pointer }.forwarding_ptr() {
            *pointer = new.as_ptr().cast();
            true
        } else {
            deferred.overlays.push(unsafe { std::ptr::read(*pointer) });
            false
        }
    });
}

/// Owns all allocations and creates objects. All objects have
/// a lifetime tied to the borrow of their `Context`. When the
/// `Context` goes out of scope, no objects should be accessible.
pub(crate) struct Context<'rt> {
    pub(crate) block: Block<false>,
    root_set: &'rt RootSet,
    next_limit: usize,
    domain_id: u64,
    owner_liveness: OwnerLiveness,
    buffer_properties: RefCell<Vec<WeakBufferPropertyRoot>>,
    // One owner-local Session for all Envs and native hosts in this Context.
    editor_gateway: OnceCell<crate::editor_document::RuntimeEditorGateway>,
    pub(in crate::core) overlay_registry: RefCell<OverlayRegistry>,
    pub(in crate::core) frame_registry: RefCell<FrameRegistry>,
    pub(in crate::core) process_registry: RefCell<ProcessRegistry>,
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        self.owner_liveness.0.store(false, Ordering::Release);
        self.overlay_registry.get_mut().shutdown();
        self.frame_registry.get_mut().shutdown();
        self.process_registry.get_mut().shutdown();
        // Global buffer handles outlive the local heap: retire their Lisp
        // metadata before releasing any allocation in this domain.
        for root in self.buffer_properties.get_mut().drain(..).filter_map(|root| root.upgrade()) {
            root.close_owned(self.domain_id);
        }
        self.garbage_collect(true);
        if self.block.objects.allocated_bytes() == 0 {
            return;
        }
        if std::thread::panicking() {
            eprintln!("Error: Context was dropped while still holding data");
        } else {
            panic!("Error: Context was dropped while still holding data");
        }
    }
}

thread_local! {
    /// Ensure there is only one context per thread.
    static SINGLETON_CHECK: Cell<bool> = const { Cell::new(false) };
}

/// Ensure there is only one global context.
static GLOBAL_CHECK: AtomicBool = AtomicBool::new(false);

impl Block<true> {
    pub(crate) fn new_global() -> Self {
        use std::sync::atomic::Ordering::SeqCst as Ord;
        assert!(GLOBAL_CHECK.compare_exchange(false, true, Ord, Ord).is_ok());
        Self::empty()
    }
}

impl Block<false> {
    pub(crate) fn new_local() -> Self {
        Self::assert_unique();
        Self::empty()
    }

    /// Construct an arena outside the thread's sole ordinary Context.
    ///
    /// SAFETY: The caller must supply an independent owner which prevents raw
    /// borrowed objects escaping into any other heap domain. Transfers must
    /// deep-copy, and collection must synchronize all retained root slots.
    pub(crate) unsafe fn new_local_unchecked() -> Self {
        Self::empty()
    }

    /// SAFETY: all new_local_unchecked ownership/synchronization obligations
    /// apply. The arena must be owned by exactly this manager identity; every
    /// endpoint copy into it must remain protected through root registration.
    pub(crate) unsafe fn new_channel_manager_unchecked(
        identity: crate::channel_manager::ChannelManagerIdentity,
    ) -> Self {
        let mut block = unsafe { Self::new_local_unchecked() };
        block.channel_manager = Some(identity);
        block
    }

    pub(crate) fn assert_unique() {
        SINGLETON_CHECK.with(|x| {
            assert!(!x.get(), "There was already and active context when this context was created");
            x.set(true);
        });
    }
}

impl<const CONST: bool> Block<CONST> {
    // No Default impl: a safe second local arena would bypass Context's domain
    // singleton and let borrowed objects be rooted in the wrong heap.
    fn empty() -> Self {
        Self {
            channel_manager: None,
            owner_domain: None,
            copy_allocations: RefCell::new(None),
            copy_walk: RefCell::new(CopyWalk::default()),
            #[cfg(test)]
            copy_failure_after: Cell::new(None),
            objects: bumpalo::Bump::new(),
            drop_stack: RefCell::default(),
            lisp_hashtables: RefCell::default(),
            lisp_chartables: RefCell::default(),
            lisp_senders: RefCell::default(),
            lisp_receivers: RefCell::default(),
            lisp_processes: RefCell::default(),
            lisp_frames: RefCell::default(),
            lisp_overlays: RefCell::default(),
            uninterned_symbol_map: UninternedSymbolMap::default(),
        }
    }

    pub(in crate::core) fn is_channel_transit(
        &self,
        identity: &crate::channel_manager::ChannelManagerIdentity,
    ) -> bool {
        !CONST && self.channel_manager.as_ref().is_some_and(|owner| owner.same(identity))
    }

    #[cfg(test)]
    pub(crate) fn tracked_char_table_allocations(&self) -> usize {
        self.lisp_chartables.borrow().len()
    }
    #[cfg(test)]
    pub(crate) fn tracked_endpoint_allocations(&self) -> (usize, usize) {
        (self.lisp_senders.borrow().len(), self.lisp_receivers.borrow().len())
    }

    #[cfg(test)]
    pub(crate) fn scoped_copy_failure(&self, successful_nodes: usize) -> impl Drop + '_ {
        let previous = self.copy_failure_after.replace(Some(successful_nodes));
        CopyFailureScope { block: self, previous }
    }
    #[cfg(test)]
    pub(crate) fn fail_copy_after(&self, successful_nodes: usize) {
        self.copy_failure_after.set(Some(successful_nodes));
    }
    #[cfg(test)]
    pub(crate) fn clear_copy_failure(&self) {
        self.copy_failure_after.set(None);
    }
    #[cfg(test)]
    pub(crate) fn check_copy_failure(&self) -> anyhow::Result<()> {
        if let Some(remaining) = self.copy_failure_after.get() {
            anyhow::ensure!(remaining != 0, "injected graph-copy failure");
            self.copy_failure_after.set(Some(remaining - 1));
        }
        Ok(())
    }

    /// Private factory brand, never inferred from caller objects or raw addresses.
    pub(in crate::core) fn belongs_to_owner(&self, owner: u64) -> bool {
        !CONST && self.owner_domain == Some(owner)
    }

    /// Intrinsic callback-free copy traversal, not a preflight snapshot. Source
    /// addresses are identity numbers only and are discarded before GC is allowed.
    pub(crate) fn enter_copy_node(&self, key: usize) -> anyhow::Result<CopyVisit<'_, CONST>> {
        let mut walk = self.copy_walk.borrow_mut();
        if walk.active.is_empty() {
            walk.work = 0;
        }
        anyhow::ensure!(walk.work < 100_000, "Graph copy exceeds expanded work budget");
        walk.work += 1;
        anyhow::ensure!(walk.active.len() < 256, "Graph copy exceeds active depth budget");
        anyhow::ensure!(!walk.active.contains(&key), "Cyclic graph copy is unsupported");
        walk.active.try_reserve(1)?;
        walk.active.push(key);
        Ok(CopyVisit { block: self, key })
    }

    /// Begin an unpublished graph-copy transaction.
    ///
    /// SAFETY: Caller excludes collection and concurrent target allocation;
    /// records only newly allocated owning wrappers, publishes no copied value
    /// before success, and commits immediately after infallible publication.
    /// Declare deferred before all manager/global locks, so native destructors
    /// run only after those guards are released. No nested transaction is allowed.
    pub(crate) unsafe fn copy_transaction<'a>(
        &'a self,
        deferred: &'a mut DeferredEndpointFinalizers,
    ) -> CopyTransaction<'a, CONST> {
        let lengths = [
            self.lisp_senders.borrow().len(),
            self.lisp_receivers.borrow().len(),
            self.lisp_processes.borrow().len(),
            self.lisp_frames.borrow().len(),
            self.lisp_hashtables.borrow().len(),
            self.lisp_chartables.borrow().len(),
            self.drop_stack.borrow().len(),
            self.lisp_overlays.borrow().len(),
        ];
        let mut active = self.copy_allocations.borrow_mut();
        assert!(active.is_none(), "nested native copy transaction");
        *active = Some(Vec::new());
        CopyTransaction { block: self, deferred, lengths, committed: false }
    }
    /// SAFETY: when a transaction is active, this must be a distinct newly
    /// allocated owning wrapper in this block, never published or registered
    /// twice. It must not be an existing same-arena identity alias.
    pub(in crate::core) unsafe fn record_copy_allocation(&self, allocation: CopyAllocation) {
        if let Some(active) = self.copy_allocations.borrow_mut().as_mut() {
            active.push(allocation);
        }
    }

    /// Scope the uninterned-symbol alias cache to one complete graph copy.
    pub(crate) fn clone_session(&self) -> impl Drop + '_ {
        self.uninterned_symbol_map.clone_session()
    }

    pub(crate) fn add<'ob, T, Tx>(&'ob self, obj: T) -> Object<'ob>
    where
        T: IntoObject<Out<'ob> = Tx>,
        Gc<Tx>: Into<Object<'ob>>,
    {
        obj.into_obj(self).into()
    }

    pub(crate) fn add_as<'ob, T, Tx, V>(&'ob self, obj: T) -> Gc<V>
    where
        T: IntoObject<Out<'ob> = Tx>,
        Gc<Tx>: Into<Gc<V>>,
    {
        obj.into_obj(self).into()
    }

    /// Create a new String whose backing storage is already part of the GC
    /// heap. Does not require dropping when moved during garbage collection
    /// (unlike std::string).
    pub(crate) fn string_with_capacity(&self, cap: usize) -> GcString<'_> {
        GcString::with_capacity_in(cap, &self.objects)
    }

    /// Create a new Vec whose backing storage is already part of the GC
    /// heap. Does not require dropping when moved during garbage collection
    /// (unlike std::vec).
    pub(crate) fn vec_new(&self) -> GcVec<'_, Object<'_>> {
        GcVec::new_in(&self.objects)
    }

    pub(crate) fn vec_with_capacity(&self, cap: usize) -> GcVec<'_, Object<'_>> {
        GcVec::with_capacity_in(cap, &self.objects)
    }
}

static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);

impl<'ob, 'rt> Context<'rt> {
    pub(crate) const MIN_GC_BYTES: usize = 2000;
    pub(crate) const GC_GROWTH_FACTOR: usize = 12; // divide by 10
    pub(crate) fn new(roots: &'rt RootSet) -> Self {
        let block = Block::new_local();
        Self::with_domain(block, roots)
    }

    fn with_domain(mut block: Block<false>, roots: &'rt RootSet) -> Self {
        let domain_id = NEXT_DOMAIN
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("Heap domain identifiers exhausted");
        block.owner_domain = Some(domain_id);
        Self {
            block,
            root_set: roots,
            next_limit: Self::MIN_GC_BYTES,
            domain_id,
            owner_liveness: OwnerLiveness(std::sync::Arc::new(AtomicBool::new(true))),
            buffer_properties: RefCell::new(Vec::new()),
            editor_gateway: OnceCell::new(),
            process_registry: RefCell::new(ProcessRegistry::default()),
            frame_registry: RefCell::new(FrameRegistry::default()),
            overlay_registry: RefCell::new(OverlayRegistry::default()),
        }
    }

    /// Native observation only, not a root, heap borrow or access credential.
    pub(crate) fn owner_liveness(&self) -> OwnerLiveness {
        self.owner_liveness.clone()
    }

    /// The initializer stores no Context reference and never invokes Lisp.
    /// Return an owned clone, never an owner/RefCell borrow across callbacks.
    /// Context teardown tombstones property stores before this native-only
    /// gateway/session is dropped; escaped clones reject every foreign owner.
    pub(crate) fn editor_gateway(
        &self,
    ) -> anyhow::Result<crate::editor_document::RuntimeEditorGateway> {
        if let Some(gateway) = self.editor_gateway.get() {
            return Ok(gateway.clone());
        }
        let gateway = crate::editor_document::RuntimeEditorGateway::new(self, 80, 24)?;
        self.editor_gateway
            .set(gateway)
            .map_err(|_| anyhow::anyhow!("Reentrant editor gateway initialization"))?;
        Ok(self.editor_gateway.get().expect("editor gateway just initialized").clone())
    }

    pub(crate) fn domain_id(&self) -> u64 {
        self.domain_id
    }
    pub(crate) fn register_buffer_properties(&self, root: WeakBufferPropertyRoot) {
        self.buffer_properties.borrow_mut().push(root);
    }

    pub(crate) fn bind<T>(&'ob self, obj: T) -> <T as WithLifetime<'ob>>::Out
    where
        T: WithLifetime<'ob>,
    {
        unsafe { obj.with_lifetime() }
    }

    pub(crate) fn get_root_set(&'ob self) -> &'rt RootSet {
        self.root_set
    }

    pub(crate) fn garbage_collect(&mut self, force: bool) {
        self.overlay_registry.get_mut().prune_dead();
        let bytes = self.block.objects.allocated_bytes();
        if cfg!(not(test)) && !force && bytes < self.next_limit {
            return;
        }

        let mut deferred = DeferredEndpointFinalizers::default();
        self.block.uninterned_symbol_map.clear();
        let mut state = GcState::new();
        for x in self.root_set.roots.borrow().iter() {
            // SAFETY: The contract of root structs will ensure that it removes
            // itself from this list before it drops.
            unsafe {
                (**x).trace(&mut state);
            }
        }

        self.buffer_properties.borrow_mut().retain(|root| {
            if let Some(root) = root.upgrade() {
                root.trace_owned(self.domain_id, &mut state);
                true
            } else {
                false
            }
        });
        state.trace_stack();

        self.next_limit = (state.to_space.allocated_bytes() * Self::GC_GROWTH_FACTOR) / 10;
        self.block.drop_stack.borrow_mut().clear();
        // Find all hashtables that have not been moved (i.e. They are no longer
        // accessible) and drop them. Otherwise, update the object pointer.
        self.block.lisp_hashtables.borrow_mut().retain_mut(|ptr| {
            let table = unsafe { &**ptr };
            if let Some(fwd) = table.forwarding_ptr() {
                *ptr = fwd.as_ptr().cast::<LispHashTable>();
                true
            } else {
                unsafe { std::ptr::drop_in_place(*ptr as *mut LispHashTable) };
                false
            }
        });

        self.block.lisp_chartables.borrow_mut().retain_mut(|ptr| {
            let table = unsafe { &**ptr };
            if let Some(fwd) = table.forwarding_ptr() {
                *ptr = fwd.as_ptr().cast::<CharTable>();
                true
            } else {
                unsafe { std::ptr::drop_in_place(*ptr as *mut CharTable) };
                false
            }
        });

        collect_endpoint_finalizers(&self.block, &mut deferred);
        self.block.objects = state.to_space;
    }
}

/// Perform garbage collection on a block with a root set without requiring a Context.
/// This is useful for scenarios like ChannelManager where we need to GC a shared block
/// from any thread without conflicting with thread-local Context singleton checks.
///
/// # Safety
/// Caller must ensure:
/// 1. Exclusive access to the block (no concurrent reads/writes)
/// 2. The root_set accurately represents all live references to objects in the block
/// 3. The block's drop_stack and native-resource allocation lists are properly maintained
/// 4. No active Context exists that references this block or root_set
/// 5. deferred is caller-owned and outlives all collector lock guards
pub(crate) unsafe fn collect_garbage_raw(
    block: &mut Block<false>,
    root_set: &ThreadSafeRootSet,
    force: bool,
    next_limit: &mut usize,
    deferred: &mut DeferredEndpointFinalizers,
) {
    let bytes = block.objects.allocated_bytes();
    if cfg!(not(test)) && !force && bytes < *next_limit {
        return;
    }

    block.uninterned_symbol_map.clear();
    let mut state = GcState::new();
    for x in root_set.roots.lock().unwrap().iter() {
        // SAFETY: The contract of root structs will ensure that it removes
        // itself from this list before it drops.
        unsafe {
            (**x).trace(&mut state);
        }
    }

    state.trace_stack();

    *next_limit = (state.to_space.allocated_bytes() * Context::GC_GROWTH_FACTOR) / 10;
    block.drop_stack.borrow_mut().clear();
    // Find all hashtables that have not been moved (i.e. They are no longer
    // accessible) and drop them. Otherwise, update the object pointer.
    block.lisp_hashtables.borrow_mut().retain_mut(|ptr| {
        let table = unsafe { &**ptr };
        if let Some(fwd) = table.forwarding_ptr() {
            *ptr = fwd.as_ptr().cast::<LispHashTable>();
            true
        } else {
            unsafe { std::ptr::drop_in_place(*ptr as *mut LispHashTable) };
            false
        }
    });

    block.lisp_chartables.borrow_mut().retain_mut(|ptr| {
        let table = unsafe { &**ptr };
        if let Some(fwd) = table.forwarding_ptr() {
            *ptr = fwd.as_ptr().cast::<CharTable>();
            true
        } else {
            unsafe { std::ptr::drop_in_place(*ptr as *mut CharTable) };
            false
        }
    });

    collect_endpoint_finalizers(block, deferred);
    block.objects = state.to_space;
}

impl Deref for Context<'_> {
    type Target = Block<false>;

    fn deref(&self) -> &Self::Target {
        &self.block
    }
}

impl AsRef<Block<false>> for Context<'_> {
    fn as_ref(&self) -> &Block<false> {
        &self.block
    }
}

impl<const CONST: bool> Drop for Block<CONST> {
    // Only one block can exist in a thread at a time. This part of that
    // contract.
    fn drop(&mut self) {
        SINGLETON_CHECK.with(|s| {
            assert!(s.get(), "Context singleton check was overwritten");
            s.set(false);
        });
    }
}

#[cfg(test)]
mod test {
    use rune_core::macros::{list, rebind, root};

    use crate::core::{
        cons::Cons,
        object::{HashTable, ObjectType, Symbol},
    };

    use super::*;
    fn bind_to_mut<'ob>(cx: &'ob mut Context) -> Object<'ob> {
        cx.add("invariant")
    }

    #[test]
    fn test_reborrow() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let obj = rebind!(bind_to_mut(cx));
        _ = "foo".into_obj(cx);
        assert_eq!(obj, "invariant");
    }

    #[test]
    fn test_garbage_collect() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(vec, new(Vec), cx);
        cx.garbage_collect(true);
        let cons = list!["foo", 1, false, "end"; cx];
        vec.push(cons);
        cx.garbage_collect(true);
    }

    #[test]
    fn test_move_values() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let int = cx.add(1);
        let float = cx.add(1.5);
        let cons: Object = Cons::new(int, float, cx).into();
        let string = cx.add("string");
        let symbol = cx.add(Symbol::new_uninterned("sym", cx));
        println!("sym: {:?}", symbol.into_raw());
        let mut table = HashTable::default();
        table.insert(symbol, string);
        let _ = table.get(&symbol).unwrap();
        root!(symbol, cx);
        let table = cx.add(table);
        let vec = vec![cons, table];
        let vec = cx.add(vec);
        root!(vec, cx);
        cx.garbage_collect(true);
        let vec = vec.bind(cx);
        let ObjectType::Vec(vec) = vec.untag() else { unreachable!() };
        let ObjectType::Cons(cons) = vec[0].get().untag() else { unreachable!() };
        let ObjectType::HashTable(table) = vec[1].get().untag() else { unreachable!() };
        let key = symbol.bind(cx);
        println!("key: {:?}", key.into_raw());
        let val = table.get(symbol.bind(cx)).unwrap();
        let ObjectType::String(string) = val.untag() else { unreachable!() };
        let ObjectType::Int(int) = cons.car().untag() else { unreachable!() };
        let ObjectType::Float(float) = cons.cdr().untag() else { unreachable!() };
        assert_eq!(string, "string");
        assert_eq!(**float, 1.5);
        assert_eq!(int, 1);
    }
}

#[cfg(test)]
mod domain_constructor_tests {
    use super::*;

    #[test]
    fn local_block_has_no_safe_default_constructor() {
        trait AmbiguousIfDefault<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfDefault<()> for T {}
        impl<T: ?Sized + Default> AmbiguousIfDefault<u8> for T {}
        let _ = <Block<false> as AmbiguousIfDefault<_>>::check;
    }

    #[test]
    fn duplicate_context_rejection_preserves_original_owner() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let other_roots = RootSet::default();
        let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _second = Context::new(&other_roots);
        }));
        assert!(second.is_err());
        assert_eq!(cx.add(7), 7);
        cx.garbage_collect(true);
    }
}

#[cfg(test)]
mod shared_editor_gateway_tests {
    use super::*;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn context_clones_one_authoritative_gateway_for_env_and_native_views() {
        let roots = RootSet::default();
        let mut cx = Context::new(&roots);
        let first = cx.editor_gateway().unwrap();
        let second = cx.editor_gateway().unwrap();
        let handle = first.scratch(&cx).unwrap();
        let old = first.read(handle, &cx).unwrap();
        let host = second.host(&cx).unwrap();
        second.splice(handle, old.epoch, 0..0, "λ🙂", &cx).unwrap();
        assert_eq!(first.read(handle, &cx).unwrap().document.to_text(), "λ🙂");
        assert_eq!(old.document.to_text(), "");
        assert!(host.snapshot().unwrap().buffers.iter().any(|b| b.document.to_text() == "λ🙂"));
        drop(host);
        cx.garbage_collect(true);
        assert_eq!(
            cx.editor_gateway().unwrap().read(handle, &cx).unwrap().document.to_text(),
            "λ🙂"
        );
    }
    #[test]
    fn escaped_gateway_clone_rejects_next_context_without_adoption() {
        let gateway = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            cx.editor_gateway().unwrap()
        };
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert!(gateway.scratch(&cx).is_err());
    }
}

#[cfg(test)]
mod owner_liveness_tests {
    use super::*;
    #[test]
    fn witness_is_native_send_sync_and_dies_before_new_owner() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<OwnerLiveness>();
        let old = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let witness = cx.owner_liveness();
            assert!(witness.is_live());
            assert!(std::thread::scope(|scope| scope.spawn(|| witness.is_live()).join().unwrap()));
            witness
        };
        assert!(!old.is_live());
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert!(cx.owner_liveness().is_live());
        assert!(!old.is_live());
    }
    #[test]
    fn retained_gateway_does_not_keep_owner_witness_alive() {
        let (gateway, witness) = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            (cx.editor_gateway().unwrap(), cx.owner_liveness())
        };
        assert!(!witness.is_live());
        drop(gateway);
    }
}
