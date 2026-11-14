//! Channel types for CSP-style concurrency.
//!
//! Channels provide communication between threads via message passing.
//! Objects are deep-copied into the ChannelManager's block when sent,
//! and deep-copied into the receiver's context when received.

use super::{CloneIn, Gc, IntoObject, Object, Symbol, TagType};
use crate::{
    channel_manager::{self, ChannelId, RecvError, SendError},
    core::{
        env::sym,
        gc::{Block, Context, GcHeap, GcState, Trace},
    },
    derive_GcMoveable,
};
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

// Re-export error types for Symbol conversion
impl From<SendError> for Symbol<'static> {
    fn from(err: SendError) -> Self {
        match err {
            SendError::Closed => sym::CHANNEL_CLOSED,
            SendError::Full => sym::CHANNEL_FULL,
            SendError::Timeout => sym::CHANNEL_TIMEOUT,
        }
    }
}

impl From<RecvError> for Symbol<'static> {
    fn from(err: RecvError) -> Self {
        match err {
            RecvError::Closed => sym::CHANNEL_CLOSED,
            RecvError::Empty => sym::CHANNEL_EMPTY,
            RecvError::Timeout => sym::CHANNEL_TIMEOUT,
        }
    }
}

/// Native family leases are shared by wrapper copies. Counts change once per
/// independently created family, and last active copy closes that family.
#[derive(Clone, Copy)]
enum EndpointKind {
    Sender,
    Receiver,
}
struct EndpointLease {
    channel_id: ChannelId,
    manager: Arc<channel_manager::ChannelManager>,
    kind: EndpointKind,
}
impl EndpointLease {
    fn new(
        channel_id: ChannelId,
        manager: Arc<channel_manager::ChannelManager>,
        kind: EndpointKind,
    ) -> Arc<Self> {
        match kind {
            EndpointKind::Sender => manager.increment_sender(channel_id),
            EndpointKind::Receiver => manager.increment_receiver(channel_id),
        }
        Arc::new(Self { channel_id, manager, kind })
    }
}
impl Drop for EndpointLease {
    fn drop(&mut self) {
        #[cfg(test)]
        self.manager.observe_endpoint_finalizer();
        match self.kind {
            EndpointKind::Sender => self.manager.close_sender(self.channel_id),
            EndpointKind::Receiver => self.manager.close_receiver(self.channel_id),
        }
        self.manager.cleanup_channel(self.channel_id);
    }
}
fn copy_lease(state: &Mutex<Option<Arc<EndpointLease>>>) -> Option<Arc<EndpointLease>> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}
fn close_lease(state: &Mutex<Option<Arc<EndpointLease>>>) {
    // No manager callback may run while this wrapper mutex is held.
    let lease = { state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() };
    drop(lease);
}

/// The sending half of a channel.
pub(crate) struct ChannelSender(pub(in crate::core) GcHeap<ChannelSenderInner>);

derive_GcMoveable!(ChannelSender);

pub(in crate::core) struct ChannelSenderInner {
    pub(in crate::core) channel_id: ChannelId,
    pub(in crate::core) manager: Arc<channel_manager::ChannelManager>,
    active: Mutex<Option<Arc<EndpointLease>>>,
}

impl ChannelSender {
    pub(in crate::core) fn new(
        channel_id: ChannelId,
        manager: Arc<channel_manager::ChannelManager>,
        constant: bool,
    ) -> Self {
        let lease = EndpointLease::new(channel_id, manager.clone(), EndpointKind::Sender);
        Self(GcHeap::new(
            ChannelSenderInner { channel_id, manager, active: Mutex::new(Some(lease)) },
            constant,
        ))
    }

    /// Send an object through the channel, blocking if full.
    /// Returns Err(SendError::Closed) if the receiver has been dropped.
    pub(crate) fn send<'ob>(&self, obj: Object<'ob>) -> Result<(), SendError> {
        let _operation = copy_lease(&self.0.active).ok_or(SendError::Closed)?;
        self.0.manager.send(self.0.channel_id, obj)
    }

    /// Try to send without blocking.
    /// Returns Err(SendError::Full) if the channel is full.
    /// Returns Err(SendError::Closed) if the receiver has been dropped.
    pub(crate) fn try_send<'ob>(&self, obj: Object<'ob>) -> Result<(), SendError> {
        let _operation = copy_lease(&self.0.active).ok_or(SendError::Closed)?;
        self.0.manager.try_send(self.0.channel_id, obj)
    }

    /// Send with a timeout.
    /// Returns Err(SendError::Timeout) if the timeout expires.
    /// Returns Err(SendError::Closed) if the receiver has been dropped.
    pub(crate) fn send_timeout<'ob>(
        &self,
        obj: Object<'ob>,
        timeout: Duration,
    ) -> Result<(), SendError> {
        let start = Instant::now();

        loop {
            match self.try_send(obj) {
                Ok(()) => return Ok(()),
                Err(SendError::Full) => {
                    if start.elapsed() >= timeout {
                        return Err(SendError::Timeout);
                    }
                    // Sleep briefly before retrying
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Explicitly close the sender side of the channel.
    /// This wakes up any receivers waiting on recv() and causes them to return Err(RecvError::Closed).
    /// This is idempotent. Already-admitted blocking operations retain a lease
    /// and may keep waiting; close does not cancel them. Future operations fail.
    pub(crate) fn close(&self) {
        close_lease(&self.0.active);
    }
}

/// The receiving half of a channel.
pub(crate) struct ChannelReceiver(pub(in crate::core) GcHeap<ChannelReceiverInner>);

derive_GcMoveable!(ChannelReceiver);

pub(in crate::core) struct ChannelReceiverInner {
    pub(in crate::core) channel_id: ChannelId,
    pub(in crate::core) manager: Arc<channel_manager::ChannelManager>,
    active: Mutex<Option<Arc<EndpointLease>>>,
}

impl ChannelReceiver {
    pub(in crate::core) fn new(
        channel_id: ChannelId,
        manager: Arc<channel_manager::ChannelManager>,
        constant: bool,
    ) -> Self {
        let lease = EndpointLease::new(channel_id, manager.clone(), EndpointKind::Receiver);
        Self(GcHeap::new(
            ChannelReceiverInner { channel_id, manager, active: Mutex::new(Some(lease)) },
            constant,
        ))
    }

    /// Receive an object from the channel, blocking if empty.
    /// Returns Err(RecvError::Closed) if all senders have been dropped.
    pub(crate) fn recv<'ob>(&self, cx: &'ob Context) -> Result<Object<'ob>, RecvError> {
        let _operation = copy_lease(&self.0.active).ok_or(RecvError::Closed)?;
        self.0.manager.recv(self.0.channel_id, &cx.block)
    }

    /// Try to receive without blocking.
    /// Returns Err(RecvError::Empty) if the channel is empty.
    /// Returns Err(RecvError::Closed) if all senders have been dropped.
    pub(crate) fn try_recv<'ob>(&self, cx: &'ob Context) -> Result<Object<'ob>, RecvError> {
        let _operation = copy_lease(&self.0.active).ok_or(RecvError::Closed)?;
        self.0.manager.try_recv(self.0.channel_id, &cx.block)
    }

    /// Receive with a timeout.
    /// Returns Err(RecvError::Timeout) if the timeout expires.
    /// Returns Err(RecvError::Closed) if all senders have been dropped.
    pub(crate) fn recv_timeout<'ob>(
        &self,
        cx: &'ob Context,
        timeout: Duration,
    ) -> Result<Object<'ob>, RecvError> {
        let start = Instant::now();

        loop {
            match self.try_recv(cx) {
                Ok(obj) => return Ok(obj),
                Err(RecvError::Empty) => {
                    if start.elapsed() >= timeout {
                        return Err(RecvError::Timeout);
                    }
                    // Sleep briefly before retrying
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Explicitly close the receiver side of the channel.
    /// This wakes up any senders waiting on send() and causes them to return Err(SendError::Closed).
    /// This is idempotent. Already-admitted blocking operations retain a lease
    /// and may keep waiting; close does not cancel them. Future operations fail.
    ///
    /// Useful for "moving" a receiver to another thread:
    /// the original receiver can be closed after being cloned into a new context.
    pub(crate) fn close(&self) {
        close_lease(&self.0.active);
    }
}

#[cfg(test)]
pub(crate) fn make_channel_pair_for_test(
    manager: Arc<channel_manager::ChannelManager>,
    capacity: usize,
) -> (ChannelId, ChannelSender, ChannelReceiver) {
    let (sender, receiver) = manager.new_channel_pair(capacity);
    (
        sender,
        ChannelSender::new(sender, manager.clone(), false),
        ChannelReceiver::new(receiver, manager, false),
    )
}

/// Create a new channel pair with the specified capacity.
pub(crate) fn make_channel_pair(capacity: usize) -> (ChannelSender, ChannelReceiver) {
    let manager = channel_manager::get_manager();
    let (sender_id, receiver_id) = manager.new_channel_pair(capacity);

    let sender = ChannelSender::new(sender_id, manager.clone(), false);
    let receiver = ChannelReceiver::new(receiver_id, manager, false);
    (sender, receiver)
}

// GC must defer owning-wrapper destruction until collector locks are released.

// GC tracing (channels themselves don't hold GC objects, state is in the manager)
impl Trace for ChannelSenderInner {
    fn trace(&self, _state: &mut GcState) {
        // No GC objects in sender
    }
}

impl Trace for ChannelReceiverInner {
    fn trace(&self, _state: &mut GcState) {
        // No GC objects in receiver
    }
}

// CloneIn implementations for cross-context copying
impl<'new> CloneIn<'new, &'new Self> for ChannelSender {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> Gc<&'new Self> {
        // The endpoint must live in the target arena, not in this stack frame.
        self.copy_owned(C).into_obj(bk)
    }
}

impl<'new> CloneIn<'new, &'new Self> for ChannelReceiver {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> Gc<&'new Self> {
        self.copy_owned(C).into_obj(bk)
    }
}

impl ChannelSender {
    fn copy_owned(&self, constant: bool) -> Self {
        Self(GcHeap::new(
            ChannelSenderInner {
                channel_id: self.0.channel_id,
                manager: self.0.manager.clone(),
                active: Mutex::new(copy_lease(&self.0.active)),
            },
            constant,
        ))
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            crate::core::gc::AllocState::Forwarded(pointer) => Some(pointer),
            crate::core::gc::AllocState::Unmoved => None,
            crate::core::gc::AllocState::Global => {
                panic!("global sender in local finalization list")
            }
        }
    }
}
impl ChannelReceiver {
    fn copy_owned(&self, constant: bool) -> Self {
        Self(GcHeap::new(
            ChannelReceiverInner {
                channel_id: self.0.channel_id,
                manager: self.0.manager.clone(),
                active: Mutex::new(copy_lease(&self.0.active)),
            },
            constant,
        ))
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            crate::core::gc::AllocState::Forwarded(pointer) => Some(pointer),
            crate::core::gc::AllocState::Unmoved => None,
            crate::core::gc::AllocState::Global => {
                panic!("global receiver in local finalization list")
            }
        }
    }
}

// IntoObject implementations are in tagged.rs

// Debug implementations
impl fmt::Debug for ChannelSender {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "#<channel-sender:{}>", self.0.channel_id)
    }
}

impl fmt::Display for ChannelSender {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "#<channel-sender>")
    }
}

impl fmt::Debug for ChannelReceiver {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "#<channel-receiver:{}>", self.0.channel_id)
    }
}

impl fmt::Display for ChannelReceiver {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "#<channel-receiver>")
    }
}

// Implement object_trait_impls macro pattern manually
impl PartialEq for ChannelSender {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(&*self.0, &*other.0)
    }
}

impl Eq for ChannelSender {}

impl PartialEq for ChannelReceiver {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(&*self.0, &*other.0)
    }
}

impl Eq for ChannelReceiver {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        cons::Cons,
        gc::{Context, RootSet},
        object::{IntoObject, ObjectType},
    };

    #[test]
    fn test_basic_send_recv() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(1);

        // Send an integer
        sender.send(cx.add(42)).unwrap();

        // Receive it
        let result = receiver.recv(&cx).unwrap();
        if let ObjectType::Int(n) = result.untag() {
            assert_eq!(n, 42);
        } else {
            panic!("Expected integer");
        }
    }

    #[test]
    fn test_double_clone_verification() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(1);

        // Create a string
        let original = cx.add("test string");

        // Send it
        sender.send(original).unwrap();

        // Receive it
        let received = receiver.recv(&cx).unwrap();

        // Verify content is same
        if let ObjectType::String(s1) = original.untag() {
            if let ObjectType::String(s2) = received.untag() {
                assert_eq!(s1.as_ref(), s2.as_ref());

                // Verify they are different allocations (double-copy worked)
                assert_ne!(
                    s1.as_ptr() as usize,
                    s2.as_ptr() as usize,
                    "Strings should be different allocations"
                );
            } else {
                panic!("Expected string");
            }
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_channel_closed_on_sender_drop() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(1);

        // Drop sender
        drop(sender);

        // Try to receive - should get Closed error
        let result = receiver.recv(&cx);
        assert!(matches!(result, Err(RecvError::Closed)));
    }

    #[test]
    fn test_channel_full() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(1);

        // Fill the channel
        sender.send(cx.add(1)).unwrap();

        // Try to send again without blocking - should get Full error
        let result = sender.try_send(cx.add(2));
        assert!(matches!(result, Err(SendError::Full)));

        // Receive the first message
        let _ = receiver.recv(&cx).unwrap();

        // Now we should be able to send again
        sender.try_send(cx.add(2)).unwrap();

        // Clean up - receive the second message
        let _ = receiver.recv(&cx).unwrap();
    }

    #[test]
    fn test_channel_empty() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (_sender, receiver) = make_channel_pair(1);

        // Try to receive from empty channel - should get Empty error
        let result = receiver.try_recv(&cx);
        assert!(matches!(result, Err(RecvError::Empty)));
    }

    #[test]
    fn test_complex_objects() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(5);

        // Send a string
        let str_obj = cx.add("hello");
        sender.send(str_obj).unwrap();

        // Send a cons cell (list)
        let list = Cons::new(cx.add(1), cx.add(2), &cx).into_obj(&cx.block);
        sender.send(list.into()).unwrap();

        // Send a vector
        let vec_obj = cx.add(vec![cx.add(10), cx.add(20), cx.add(30)]);
        sender.send(vec_obj).unwrap();

        // Receive and verify string
        let recv_str = receiver.recv(&cx).unwrap();
        if let ObjectType::String(s) = recv_str.untag() {
            assert_eq!(s.as_ref(), "hello");
        } else {
            panic!("Expected string");
        }

        // Receive and verify cons
        let recv_list = receiver.recv(&cx).unwrap();
        if let ObjectType::Cons(cons) = recv_list.untag() {
            let car_obj = cons.car();
            if let ObjectType::Int(car) = car_obj.untag() {
                assert_eq!(car, 1);
            } else {
                panic!("Expected int in car");
            }
        } else {
            panic!("Expected cons");
        }

        // Receive and verify vector
        let recv_vec = receiver.recv(&cx).unwrap();
        if let ObjectType::Vec(v) = recv_vec.untag() {
            assert_eq!(v.len(), 3);
            if let ObjectType::Int(n) = v[0].get().untag() {
                assert_eq!(n, 10);
            }
        } else {
            panic!("Expected vector");
        }
    }

    #[test]
    fn test_timeout() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (_sender, receiver) = make_channel_pair(1);

        // Try to receive with very short timeout - should timeout
        let result = receiver.recv_timeout(&cx, Duration::from_millis(10));
        assert!(matches!(result, Err(RecvError::Timeout)));
    }

    #[test]
    fn test_multiple_messages() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(10);

        // Send multiple messages
        for i in 0..10 {
            sender.send(cx.add(i)).unwrap();
        }

        // Receive them in order
        for i in 0..10 {
            let result = receiver.recv(&cx).unwrap();
            if let ObjectType::Int(n) = result.untag() {
                assert_eq!(n, i);
            } else {
                panic!("Expected integer");
            }
        }
    }

    #[test]
    fn test_channel_sender_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ChannelSender>();
    }

    #[test]
    fn test_channel_receiver_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ChannelReceiver>();
    }

    #[test]
    fn test_cross_thread_channel() {
        use std::thread;

        let roots = RootSet::default();
        let cx = Context::new(&roots);

        let (sender, receiver) = make_channel_pair(5);

        // Send from spawned thread
        let handle = thread::spawn(move || {
            let roots2 = RootSet::default();
            let cx2 = Context::new(&roots2);
            for i in 0..5 {
                sender.send(cx2.add(i * 10)).unwrap();
            }
        });

        // Receive in main thread
        for i in 0..5 {
            let result = receiver.recv(&cx).unwrap();
            if let ObjectType::Int(n) = result.untag() {
                assert_eq!(n, i * 10);
            } else {
                panic!("Expected integer");
            }
        }

        handle.join().unwrap();
    }

    #[test]
    fn test_concurrent_channel_stress() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let roots = RootSet::default();
        let _cx = Context::new(&roots);

        let send_count = Arc::new(AtomicUsize::new(0));
        let recv_count = Arc::new(AtomicUsize::new(0));

        // Spawn multiple sender/receiver thread pairs
        let mut handles = vec![];
        for idx in 0..4 {
            let (sender, receiver) = make_channel_pair(10);
            let send_counter = Arc::clone(&send_count);
            let recv_counter = Arc::clone(&recv_count);

            // Sender thread
            handles.push(thread::spawn(move || {
                let roots = RootSet::default();
                let cx = Context::new(&roots);
                for i in 0..20 {
                    let value = (idx * 100 + i) as i64;
                    sender.send(cx.add(value)).unwrap();
                    send_counter.fetch_add(1, Ordering::SeqCst);
                }
            }));

            // Receiver thread
            handles.push(thread::spawn(move || {
                let roots = RootSet::default();
                let cx = Context::new(&roots);
                for _ in 0..20 {
                    let result = receiver.recv(&cx).unwrap();
                    // Verify it's an integer
                    assert!(matches!(result.untag(), ObjectType::Int(_)));
                    recv_counter.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all messages were sent and received
        assert_eq!(send_count.load(Ordering::SeqCst), 80);
        assert_eq!(recv_count.load(Ordering::SeqCst), 80);
    }
}

#[cfg(test)]
mod endpoint_ownership_tests {
    use super::*;
    use crate::core::{
        env::INTERNED_SYMBOLS,
        gc::{AllocState, RootSet},
    };
    use rune_core::macros::root;

    fn assert_in_target_arena<T: Copy>(value: Gc<T>, block: &Block<false>, size: usize) {
        // Decode the tagged address without creating/dereferencing a reference:
        // the old dangling-stack implementation must fail this check safely.
        let address = ((value.into_raw().addr() as isize) >> 8) as usize;
        // SAFETY: No arena allocation occurs during this raw chunk iteration;
        // only integer address ranges are inspected, never the chunk contents.
        let owned = unsafe { block.objects.iter_allocated_chunks_raw() }.any(|(start, len)| {
            let start = start as usize;
            address >= start && address.checked_add(size).is_some_and(|end| end <= start + len)
        });
        assert!(owned, "cloned endpoint was not allocated in target arena");
    }

    #[test]
    fn cloned_endpoints_are_arena_owned_and_survive_repeated_gc() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let (sender, receiver) = make_channel_pair(1);
        let copied_sender = sender.clone_in(&cx.block);
        let copied_receiver = receiver.clone_in(&cx.block);
        assert_in_target_arena(copied_sender, &cx.block, size_of::<ChannelSender>());
        assert_in_target_arena(copied_receiver, &cx.block, size_of::<ChannelReceiver>());
        root!(copied_sender, cx);
        root!(copied_receiver, cx);
        for value in 0..10 {
            cx.garbage_collect(true);
            copied_sender.bind(cx).send(cx.add(value)).unwrap();
            assert_eq!(copied_receiver.bind(cx).recv(cx).unwrap(), value);
        }
        // This regression covers arena ownership; lifecycle tests cover GC close.
        copied_sender.bind(cx).close();
        copied_receiver.bind(cx).close();
        sender.close();
        receiver.close();
    }

    #[test]
    fn endpoint_payloads_roundtrip_through_channel_copy_domains() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let (sender, receiver) = make_channel_pair(1);
        let (carrier_sender, carrier_receiver) = make_channel_pair(1);
        let sender = sender.into_obj(&cx.block);
        root!(sender, cx);
        carrier_sender.send(cx.add(sender.bind(cx))).unwrap();
        let copied = carrier_receiver.recv(cx).unwrap();
        root!(copied, cx);
        for value in 0..5 {
            cx.garbage_collect(true);
            let endpoint: Gc<&ChannelSender> = copied.bind(cx).try_into().unwrap();
            endpoint.send(cx.add(value)).unwrap();
            assert_eq!(receiver.recv(cx).unwrap(), value);
        }
        let receiver = receiver.into_obj(&cx.block);
        root!(receiver, cx);
        carrier_sender.send(cx.add(receiver.bind(cx))).unwrap();
        let copied_receiver = carrier_receiver.recv(cx).unwrap();
        root!(copied_receiver, cx);
        for value in 5..10 {
            cx.garbage_collect(true);
            sender.bind(cx).send(cx.add(value)).unwrap();
            let endpoint: Gc<&ChannelReceiver> = copied_receiver.bind(cx).try_into().unwrap();
            assert_eq!(endpoint.recv(cx).unwrap(), value);
        }
        copied
            .bind(cx)
            .try_into()
            .map(|endpoint: Gc<&ChannelSender>| endpoint.close())
            .unwrap();
        copied_receiver
            .bind(cx)
            .try_into()
            .map(|endpoint: Gc<&ChannelReceiver>| endpoint.close())
            .unwrap();
        sender.bind(cx).close();
        receiver.bind(cx).close();
        // Transit wrappers finalize outside the manager's collector locks.
        sender.bind(cx).untag().0.manager.collect_for_test();
        carrier_sender.close();
        carrier_receiver.close();
    }

    #[test]
    fn owned_endpoint_conversion_uses_global_target_marker() {
        let roots = RootSet::default();
        let _cx = Context::new(&roots);
        let (sender, receiver) = make_channel_pair(1);
        let map = INTERNED_SYMBOLS.lock().unwrap();
        let sender = sender.into_obj(map.global_block());
        let receiver = receiver.into_obj(map.global_block());
        assert!(matches!(sender.untag().0.allocation_state(), AllocState::Global));
        assert!(matches!(receiver.untag().0.allocation_state(), AllocState::Global));
        sender.close();
        receiver.close();
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::core::{gc::RootSet, object::ObjectType};
    use rune_core::macros::root;

    #[test]
    fn close_drop_and_closed_clone_cannot_close_a_surviving_alias() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let alias = sender.clone_in(&cx.block);
        root!(alias, cx);
        assert_eq!(manager.counts_for_test(id), Some((1, 1)));
        sender.close();
        sender.close();
        drop(sender);
        assert_eq!(manager.counts_for_test(id), Some((1, 1)));
        alias.bind(cx).send(cx.add(7)).unwrap();
        assert_eq!(receiver.recv(cx).unwrap(), 7);
        alias.bind(cx).close();
        alias.bind(cx).close();
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        let closed = alias.bind(cx).clone_in(&cx.block);
        assert_eq!(closed.try_send(cx.add(8)), Err(SendError::Closed));
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        receiver.close();
        receiver.close();
        drop(receiver);
        assert_eq!(manager.counts_for_test(id), None);
        cx.garbage_collect(true);
    }
    #[test]
    fn receiver_close_is_idempotent_and_last_alias_releases_family() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let alias = receiver.clone_in(&cx.block);
        root!(alias, cx);
        receiver.close();
        receiver.close();
        drop(receiver);
        sender.send(cx.add(9)).unwrap();
        assert_eq!(alias.bind(cx).recv(cx).unwrap(), 9);
        alias.bind(cx).close();
        let closed = alias.bind(cx).clone_in(&cx.block);
        assert_eq!(closed.try_recv(cx), Err(RecvError::Closed));
        assert_eq!(manager.counts_for_test(id), Some((1, 0)));
        sender.close();
        drop(sender);
        assert_eq!(manager.counts_for_test(id), None);
        cx.garbage_collect(true);
    }
    #[test]
    fn self_channel_endpoint_roundtrip_does_not_reenter_data_lock() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let sender = sender.into_obj(&cx.block);
        root!(sender, cx);
        sender.bind(cx).send(cx.add(sender.bind(cx))).unwrap();
        let copy = receiver.recv(cx).unwrap();
        root!(copy, cx);
        let ObjectType::ChannelSender(copied) = copy.bind(cx).untag() else {
            panic!("sender expected")
        };
        copied.send(cx.add(11)).unwrap();
        assert_eq!(receiver.recv(cx).unwrap(), 11);
        manager.collect_for_test();
        copied.close();
        sender.bind(cx).close();
        receiver.close();
        cx.garbage_collect(true);
        manager.collect_for_test();
        assert_eq!(manager.counts_for_test(id), None);
    }
    #[test]
    fn self_channel_receiver_roundtrip_preserves_a_live_alias() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let receiver = receiver.into_obj(&cx.block);
        root!(receiver, cx);
        sender.send(cx.add(receiver.bind(cx))).unwrap();
        let copy = receiver.bind(cx).recv(cx).unwrap();
        root!(copy, cx);
        receiver.bind(cx).close();
        sender.send(cx.add(13)).unwrap();
        let copied: Gc<&ChannelReceiver> = copy.bind(cx).try_into().unwrap();
        assert_eq!(copied.recv(cx).unwrap(), 13);
        copied.close();
        sender.close();
        cx.garbage_collect(true);
        manager.collect_for_test();
        assert_eq!(manager.counts_for_test(id), None);
    }
    #[test]
    fn opposite_carrier_transfers_do_not_form_a_data_lock_cycle() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (a, a_sender, a_receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let (b, b_sender, b_receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let left_carrier = a_sender.copy_owned(false);
        let left_payload = b_sender.copy_owned(false);
        let right_carrier = b_sender.copy_owned(false);
        let right_payload = a_sender.copy_owned(false);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (done, completions) = std::sync::mpsc::channel();
        let start = barrier.clone();
        let finished = done.clone();
        let left = std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            let payload = left_payload.into_obj(&cx.block);
            root!(payload, cx);
            start.wait();
            left_carrier.send(cx.add(payload.bind(cx))).unwrap();
            finished.send(()).unwrap();
        });
        let start = barrier.clone();
        let right = std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            let payload = right_payload.into_obj(&cx.block);
            root!(payload, cx);
            start.wait();
            right_carrier.send(cx.add(payload.bind(cx))).unwrap();
            done.send(()).unwrap();
        });
        barrier.wait();
        for _ in 0..2 {
            if completions.recv_timeout(Duration::from_secs(3)).is_err() {
                std::mem::forget(a_receiver);
                std::mem::forget(b_receiver);
                panic!("opposite endpoint transfers deadlocked");
            }
        }
        left.join().unwrap();
        right.join().unwrap();
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let payload = a_receiver.recv(&cx).unwrap();
        let endpoint: Gc<&ChannelSender> = payload.try_into().unwrap();
        endpoint.close();
        let payload = b_receiver.recv(&cx).unwrap();
        let endpoint: Gc<&ChannelSender> = payload.try_into().unwrap();
        endpoint.close();
        a_sender.close();
        b_sender.close();
        a_receiver.close();
        b_receiver.close();
        manager.collect_for_test();
        assert_eq!(manager.counts_for_test(a), None);
        assert_eq!(manager.counts_for_test(b), None);
    }
    #[test]
    fn cloning_racing_close_cannot_resurrect_a_native_family() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let state = &sender.0.active;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..500 {
                    drop(copy_lease(state));
                }
            });
            scope.spawn(|| {
                for _ in 0..500 {
                    close_lease(state);
                }
            });
        });
        assert!(copy_lease(state).is_none());
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert_eq!(sender.try_send(cx.add(1)), Err(SendError::Closed));
        drop(sender);
        receiver.close();
        assert_eq!(manager.counts_for_test(id), None);
    }
    #[test]
    fn owner_gc_finalizes_last_unclosed_heap_endpoint_once() {
        let manager = channel_manager::ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        {
            let sender = sender.into_obj(&cx.block);
            root!(sender, cx);
            for _ in 0..5 {
                cx.garbage_collect(true);
                assert_eq!(manager.counts_for_test(id), Some((1, 1)));
            }
        }
        cx.garbage_collect(true);
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        assert_eq!(receiver.try_recv(cx), Err(RecvError::Closed));
        cx.garbage_collect(true);
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
        receiver.close();
        assert_eq!(manager.counts_for_test(id), None);
    }
}
