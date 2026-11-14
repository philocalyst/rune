//! Channel Manager - manages all channel state in a dedicated garbage-collected block
//!
//! This module provides a singleton ChannelManager that
//! can be accessed from multiple threads.
//! It has its own Block and manages its roots manually using the Heap.
//! All channel operations go through this manager, allowing proper garbage
//! collection of in-transit objects.

use crate::core::{
    gc::{
        __HeapRoot, Block, Context, DeferredEndpointFinalizers, Slot, ThreadSafeRootSet,
        collect_garbage_raw,
    },
    object::{CloneIn, Object},
};
use std::sync::OnceLock;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

/// Unique identifier for a channel
pub(crate) type ChannelId = u64;

/// GC state for the channel manager, all guarded by a single Mutex to prevent deadlocks
struct GcState {
    /// The block where all in-transit objects are stored
    block: Block<false>,
    /// Static reference to the root set (points into gc_state.roots)
    roots_static: &'static ThreadSafeRootSet,
    /// Next GC threshold (in bytes)
    next_limit: usize,
}

impl GcState {
    fn new(roots_static: &'static ThreadSafeRootSet) -> Self {
        Self {
            // SAFETY: ChannelManager exclusively owns this arena under its
            // gc_state mutex and deep-copies every incoming/outgoing value.
            block: unsafe { Block::new_local_unchecked() },
            roots_static,
            next_limit: Context::MIN_GC_BYTES,
        }
    }
}

/// Data for a single channel stored in the manager
struct ChannelData {
    /// Queue of objects rooted via HeapRoot
    queue: VecDeque<__HeapRoot<Slot<Object<'static>>>>,
    /// Maximum capacity of the channel
    capacity: usize,
    /// Active native sender lease families, not wrapper copies
    sender_count: usize,
    /// Active native receiver lease families
    receiver_count: usize,
    /// Whether senders have been explicitly closed
    sender_closed: bool,
    /// Whether receivers have been explicitly closed
    receiver_closed: bool,
}

impl ChannelData {
    fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            sender_count: 0,
            receiver_count: 0,
            sender_closed: false,
            receiver_closed: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn is_full(&self) -> bool {
        self.queue.len() >= self.capacity
    }
}

/// State for a single channel, with synchronization
struct ChannelState {
    data: Mutex<ChannelData>,
    /// Condvar to wake senders waiting for space
    not_full: Condvar,
    /// Condvar to wake receivers waiting for data
    not_empty: Condvar,
}

impl ChannelState {
    fn new(capacity: usize) -> Self {
        Self {
            data: Mutex::new(ChannelData::new(capacity)),
            not_full: Condvar::new(),
            not_empty: Condvar::new(),
        }
    }
}

/// The channel manager state
pub(crate) struct ChannelManager {
    /// GC state (block, roots, and next_limit) all guarded by a single Mutex
    /// We also leak a static reference to the RootSet for HeapRoot creation
    gc_state: Mutex<GcState>,
    /// Ensures mutual exclusion between GC and channel operations.
    /// Channel operations acquire read lock when accessing rooted data,
    /// GC acquires write lock for exclusive access during trace.
    gc_access: RwLock<()>,
    /// All channels managed by this manager
    channels: RwLock<HashMap<ChannelId, Arc<ChannelState>>>,
    /// Counter for generating unique channel IDs
    next_channel_id: AtomicU64,
    /// Counter for receives since last GC
    recv_count: AtomicUsize,
    /// Trigger GC after this many receives
    gc_threshold: usize,
    #[cfg(test)]
    probe_finalizer_locks: AtomicBool,
    #[cfg(test)]
    finalizer_lock_violation: AtomicBool,
}

impl ChannelManager {
    fn new() -> Self {
        // It is safe to leak the rootset here, `ChannelManager` is intended
        // to be lazily instantiated once for the whole remaining lifetime of
        // the program
        let roots_static: &'static ThreadSafeRootSet =
            Box::leak(Box::new(ThreadSafeRootSet::default()));

        Self {
            gc_state: Mutex::new(GcState::new(roots_static)),
            gc_access: RwLock::new(()),
            channels: RwLock::new(HashMap::new()),
            next_channel_id: AtomicU64::new(1),
            recv_count: AtomicUsize::new(0),
            gc_threshold: 10,
            #[cfg(test)]
            probe_finalizer_locks: AtomicBool::new(false),
            #[cfg(test)]
            finalizer_lock_violation: AtomicBool::new(false),
        }
    }

    /// Create a new channel and return its ID
    fn create_channel(&self, capacity: usize) -> ChannelId {
        let id = self.next_channel_id.fetch_add(1, Ordering::SeqCst);
        let mut channels = self.channels.write().unwrap();
        channels.insert(id, Arc::new(ChannelState::new(capacity)));
        id
    }

    /// Register one newly created native sender family (not a wrapper clone)
    pub(crate) fn increment_sender(&self, channel_id: ChannelId) {
        let channels = self.channels.read().unwrap();
        if let Some(state) = channels.get(&channel_id) {
            let mut data = state.data.lock().unwrap();
            data.sender_count += 1;
        }
    }

    /// Register one newly created native receiver family
    pub(crate) fn increment_receiver(&self, channel_id: ChannelId) {
        let channels = self.channels.read().unwrap();
        if let Some(state) = channels.get(&channel_id) {
            let mut data = state.data.lock().unwrap();
            data.receiver_count += 1;
        }
    }

    fn channel(&self, id: ChannelId) -> Option<Arc<ChannelState>> {
        self.channels.read().unwrap().get(&id).cloned()
    }
    fn send_closed(data: &ChannelData) -> bool {
        data.sender_count == 0
            || data.sender_closed
            || data.receiver_count == 0
            || data.receiver_closed
    }
    /// Copy without ChannelData locks; a payload may reference its own carrier.
    fn copy_for_queue(&self, object: Object) -> __HeapRoot<Slot<Object<'static>>> {
        let _access = self.gc_access.read().unwrap();
        let state = self.gc_state.lock().unwrap();
        let _session = state.block.clone_session();
        let copied = object.clone_in(&state.block);
        // SAFETY: register the manager-owned graph before releasing gc_access.
        let copied = unsafe { std::mem::transmute::<Object<'_>, Object<'static>>(copied) };
        __HeapRoot::new(Slot::new(copied), state.roots_static)
    }
    fn reject_prepared(&self, prepared: __HeapRoot<Slot<Object<'static>>>) {
        drop(prepared);
        // Rejected copied endpoints must not keep channels alive until unrelated
        // traffic. Caller has released data locks before this forced collection.
        self.collect_garbage_with_force(true);
    }
    pub(crate) fn try_send(&self, id: ChannelId, object: Object) -> Result<(), SendError> {
        let state = self.channel(id).ok_or(SendError::Closed)?;
        {
            let data = state.data.lock().unwrap();
            if Self::send_closed(&data) {
                return Err(SendError::Closed);
            }
            if data.is_full() {
                return Err(SendError::Full);
            }
        }
        let prepared = self.copy_for_queue(object);
        let mut data = state.data.lock().unwrap();
        let rejected = if Self::send_closed(&data) {
            Some(SendError::Closed)
        } else if data.is_full() {
            Some(SendError::Full)
        } else {
            None
        };
        if let Some(error) = rejected {
            drop(data);
            self.reject_prepared(prepared);
            return Err(error);
        }
        data.queue.push_back(prepared);
        drop(data);
        state.not_empty.notify_one();
        Ok(())
    }
    pub(crate) fn send(&self, id: ChannelId, object: Object) -> Result<(), SendError> {
        let state = self.channel(id).ok_or(SendError::Closed)?;
        {
            let mut data = state.data.lock().unwrap();
            while data.is_full() && !Self::send_closed(&data) {
                data = state.not_full.wait(data).unwrap();
            }
            if Self::send_closed(&data) {
                return Err(SendError::Closed);
            }
        }
        let prepared = self.copy_for_queue(object);
        let mut data = state.data.lock().unwrap();
        // Optimistic preparation: revalidate capacity and retain a HeapRoot,
        // not a gc_access guard, while blocked. Collection must keep progressing.
        while data.is_full() && !Self::send_closed(&data) {
            data = state.not_full.wait(data).unwrap();
        }
        if Self::send_closed(&data) {
            drop(data);
            self.reject_prepared(prepared);
            return Err(SendError::Closed);
        }
        data.queue.push_back(prepared);
        drop(data);
        state.not_empty.notify_one();
        Ok(())
    }
    fn receive_prepared<'ob>(
        &self,
        rooted: __HeapRoot<Slot<Object<'static>>>,
        target: &'ob Block<false>,
    ) -> Object<'ob> {
        let result = {
            let _access = self.gc_access.read().unwrap();
            // SAFETY: root stays alive and access excludes transit collection
            // throughout the target graph copy. No data lock is held.
            let object = unsafe { rooted.get_inner() };
            let _session = target.clone_session();
            object.clone_in(target)
        };
        drop(rooted);
        if self.recv_count.fetch_add(1, Ordering::SeqCst) + 1 >= self.gc_threshold {
            self.recv_count.store(0, Ordering::SeqCst);
            self.collect_garbage();
        }
        result
    }
    pub(crate) fn try_recv<'ob>(
        &self,
        id: ChannelId,
        target: &'ob Block<false>,
    ) -> Result<Object<'ob>, RecvError> {
        let state = self.channel(id).ok_or(RecvError::Closed)?;
        let mut data = state.data.lock().unwrap();
        if data.receiver_closed || data.receiver_count == 0 {
            return Err(RecvError::Closed);
        }
        let rooted = match data.queue.pop_front() {
            Some(rooted) => rooted,
            None if data.sender_count == 0 || data.sender_closed => return Err(RecvError::Closed),
            None => return Err(RecvError::Empty),
        };
        drop(data);
        state.not_full.notify_one();
        Ok(self.receive_prepared(rooted, target))
    }
    pub(crate) fn recv<'ob>(
        &self,
        id: ChannelId,
        target: &'ob Block<false>,
    ) -> Result<Object<'ob>, RecvError> {
        let state = self.channel(id).ok_or(RecvError::Closed)?;
        let mut data = state.data.lock().unwrap();
        while data.is_empty() {
            if data.receiver_closed
                || data.receiver_count == 0
                || data.sender_count == 0
                || data.sender_closed
            {
                return Err(RecvError::Closed);
            }
            data = state.not_empty.wait(data).unwrap();
        }
        if data.receiver_closed || data.receiver_count == 0 {
            return Err(RecvError::Closed);
        }
        let rooted = data.queue.pop_front().expect("nonempty queue");
        drop(data);
        state.not_full.notify_one();
        Ok(self.receive_prepared(rooted, target))
    }

    /// Close the sender side of a channel
    pub(crate) fn close_sender(&self, channel_id: ChannelId) {
        let channels = self.channels.read().unwrap();
        if let Some(state) = channels.get(&channel_id) {
            let mut data = state.data.lock().unwrap();
            if data.sender_count > 0 {
                data.sender_count -= 1;
            }
            if data.sender_count == 0 {
                data.sender_closed = true;
                drop(data);
                state.not_empty.notify_all();
            }
        }
    }

    /// Close the receiver side of a channel
    pub(crate) fn close_receiver(&self, channel_id: ChannelId) {
        let channels = self.channels.read().unwrap();
        if let Some(state) = channels.get(&channel_id) {
            let mut data = state.data.lock().unwrap();
            if data.receiver_count > 0 {
                data.receiver_count -= 1;
            }
            if data.receiver_count == 0 {
                data.receiver_closed = true;
                // Only root membership is dropped, never Lisp values. Root
                // removal synchronizes with trace through roots mutex. No data
                // -> gc_access read acquisition (writer-preference inversion).
                data.queue.clear();
                drop(data);
                state.not_full.notify_all();
                state.not_empty.notify_all();
            }
        }
    }

    fn collect_garbage(&self) {
        self.collect_garbage_with_force(false);
    }
    fn collect_garbage_with_force(&self, force: bool) {
        // Declare BEFORE guards: callbacks must be outside collector locks on
        // both normal return and unwind after completed raw collection.
        let mut deferred = DeferredEndpointFinalizers::default();
        {
            let _access = self.gc_access.write().unwrap();
            let mut state = self.gc_state.lock().unwrap();
            let GcState { ref mut block, ref mut next_limit, roots_static } = *state;
            unsafe {
                collect_garbage_raw(block, roots_static, force, next_limit, &mut deferred);
            }
        }
        drop(deferred);
    }
    #[cfg(test)]
    pub(crate) fn collect_for_test(&self) {
        self.collect_garbage_with_force(true);
    }
    #[cfg(test)]
    pub(crate) fn isolated_for_test() -> Arc<Self> {
        let manager = Arc::new(Self::new());
        // Match the process-lifetime manager/root-set model; do not drop an
        // unchecked native arena as though it owned the Context singleton.
        std::mem::forget(manager.clone());
        manager
    }
    #[cfg(test)]
    pub(crate) fn counts_for_test(&self, id: ChannelId) -> Option<(usize, usize)> {
        let state = self.channel(id)?;
        let data = state.data.lock().unwrap();
        Some((data.sender_count, data.receiver_count))
    }
    #[cfg(test)]
    pub(crate) fn observe_endpoint_finalizer(&self) {
        if !self.probe_finalizer_locks.load(Ordering::Relaxed) {
            return;
        }
        use std::sync::TryLockError::WouldBlock;
        let state_free = !matches!(self.gc_state.try_lock(), Err(WouldBlock));
        let access_free = !matches!(self.gc_access.try_read(), Err(WouldBlock));
        if !state_free || !access_free {
            self.finalizer_lock_violation.store(true, Ordering::Relaxed);
        }
    }

    /// Remove a channel that has no more senders or receivers
    pub(crate) fn cleanup_channel(&self, channel_id: ChannelId) {
        let channels = self.channels.read().unwrap();
        let should_remove = if let Some(state) = channels.get(&channel_id) {
            let data = state.data.lock().unwrap();
            data.sender_count == 0 && data.receiver_count == 0
        } else {
            false
        };
        drop(channels);

        if should_remove {
            let mut channels = self.channels.write().unwrap();
            channels.remove(&channel_id);
        }
    }
}

// SAFETY: ChannelManager is safe to Send/Sync across threads because:
// 1. All state is protected by Mutex or is Atomic
// 2. The Block<false> contains RefCell allocation/finalization lists, but:
//    - These are only accessed during GC, which holds the gc_state mutex
//    - The mutex ensures no concurrent access to these RefCells
//    - No references to Block internals escape the mutex guard
// 3. The ThreadSafeRootSet uses Mutex internally and is 'static
// 4. HeapRoot instances in channel queues do not independently implement
//    Send/Sync for Object slots. They are confined to this manager's locks;
//    public send/receive operations deep-copy into/out of the manager block.
// 5. All public methods are designed to be called concurrently from multiple threads

unsafe impl Send for ChannelManager {}
unsafe impl Sync for ChannelManager {}

/// Global singleton channel manager
static CHANNEL_MANAGER: OnceLock<Arc<ChannelManager>> = OnceLock::new();

/// Get a reference to the global channel manager
pub(crate) fn get_manager() -> Arc<ChannelManager> {
    Arc::clone(CHANNEL_MANAGER.get_or_init(|| Arc::new(ChannelManager::new())))
}

/// Error type for send operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendError {
    /// Receiver has been dropped
    Closed,
    /// Channel is full (only for try_send)
    Full,
    /// Send operation timed out
    Timeout,
}

/// Error type for receive operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecvError {
    /// All senders have been dropped
    Closed,
    /// Channel is empty (only for try_recv)
    Empty,
    /// Receive operation timed out
    Timeout,
}

// Public API for channel operations
impl ChannelManager {
    pub(crate) fn new_channel_pair(&self, capacity: usize) -> (ChannelId, ChannelId) {
        let id = self.create_channel(capacity);
        (id, id)
    }
}

#[cfg(test)]
mod deferred_endpoint_tests {
    use super::*;
    use crate::core::object::{IntoObject, make_channel_pair_for_test};
    fn unreachable_sender(
        manager: &Arc<ChannelManager>,
    ) -> (ChannelId, crate::core::object::ChannelReceiver) {
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let _access = manager.gc_access.write().unwrap();
        let state = manager.gc_state.lock().unwrap();
        let _sender = sender.into_obj(&state.block);
        (id, receiver)
    }
    #[test]
    fn raw_gc_defers_native_close_until_batch_and_locks_are_released() {
        let manager = ChannelManager::isolated_for_test();
        manager.probe_finalizer_locks.store(true, Ordering::Relaxed);
        let (id, receiver) = unreachable_sender(&manager);
        let mut deferred = DeferredEndpointFinalizers::default();
        {
            let _access = manager.gc_access.write().unwrap();
            let mut state = manager.gc_state.lock().unwrap();
            let GcState { ref mut block, roots_static, ref mut next_limit } = *state;
            unsafe {
                collect_garbage_raw(block, roots_static, true, next_limit, &mut deferred);
            }
            assert_eq!(manager.counts_for_test(id), Some((1, 1)), "callback under collector locks");
            assert_eq!(block.tracked_endpoint_allocations(), (0, 0));
        }
        assert_eq!(manager.counts_for_test(id), Some((1, 1)));
        drop(deferred);
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        assert!(!manager.finalizer_lock_violation.load(Ordering::Relaxed));
        receiver.close();
        assert_eq!(manager.counts_for_test(id), None);
    }
    #[test]
    fn unwind_drops_collector_guards_before_deferred_native_callbacks() {
        let manager = ChannelManager::isolated_for_test();
        manager.probe_finalizer_locks.store(true, Ordering::Relaxed);
        let (id, receiver) = unreachable_sender(&manager);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut deferred = DeferredEndpointFinalizers::default();
            let _access = manager.gc_access.write().unwrap();
            let mut state = manager.gc_state.lock().unwrap();
            let GcState { ref mut block, roots_static, ref mut next_limit } = *state;
            unsafe {
                collect_garbage_raw(block, roots_static, true, next_limit, &mut deferred);
            }
            assert_eq!(manager.counts_for_test(id), Some((1, 1)));
            panic!("after raw collection, before normal guard release");
        }));
        assert!(result.is_err());
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        assert!(!manager.finalizer_lock_violation.load(Ordering::Relaxed));
        receiver.close();
    }
}

#[cfg(test)]
mod char_table_finalization_tests {
    use super::*;
    use crate::core::{
        gc::RootSet,
        object::{CharTableInner, IntoObject, ObjectType, finalized_char_table_count},
    };
    use rune_core::macros::root;
    #[test]
    fn manager_raw_gc_moves_then_finalizes_transferred_char_table_exactly_once() {
        let manager = ChannelManager::isolated_for_test();
        let channel = manager.create_channel(1);
        manager.increment_sender(channel);
        manager.increment_receiver(channel);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let before = finalized_char_table_count();
        {
            let source = CharTableInner::new(None).into_obj(&cx.block);
            root!(source, cx);
            source.bind(cx).untag().set(42, cx.add("transferred native map")).unwrap();
            manager.try_send(channel, cx.add(source.bind(cx))).unwrap();
            for _ in 0..5 {
                manager.collect_garbage();
                assert_eq!(finalized_char_table_count(), before);
                assert_eq!(
                    manager.gc_state.lock().unwrap().block.tracked_char_table_allocations(),
                    1
                );
            }
            let received = manager.try_recv(channel, &cx.block).unwrap();
            root!(received, cx);
            let ObjectType::CharTable(table) = received.bind(cx).untag() else {
                panic!("table expected")
            };
            assert_eq!(table.get(42), "transferred native map");
            manager.collect_garbage();
            assert_eq!(finalized_char_table_count(), before + 1);
            assert_eq!(manager.gc_state.lock().unwrap().block.tracked_char_table_allocations(), 0);
            cx.garbage_collect(true);
            assert_eq!(finalized_char_table_count(), before + 1);
            assert_eq!(cx.block.tracked_char_table_allocations(), 2);
        }
        cx.garbage_collect(true);
        assert_eq!(finalized_char_table_count(), before + 3);
        assert_eq!(cx.block.tracked_char_table_allocations(), 0);
        manager.collect_garbage();
        assert_eq!(finalized_char_table_count(), before + 3);
        manager.close_sender(channel);
        manager.close_receiver(channel);
        manager.cleanup_channel(channel);
    }
}
