//! Unpublished graph-copy rollback: native leases must retire before arena death.
use super::{Context, DeferredEndpointFinalizers, RootSet};
use crate::{
    channel_manager::{ChannelManager, RecvError},
    core::object::{CloneIn, ObjectType, make_channel_pair_for_test},
};

fn nested_endpoint_failure(panic_after_copy: bool) {
    let roots = RootSet::default();
    let source = Context::new(&roots);
    let target = &source.block;
    let manager = ChannelManager::isolated_for_test();
    let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
    let sender = source.add(sender);
    let graph = source.add(vec![sender, source.add(42)]);
    let before = target.tracked_endpoint_allocations();
    let mut deferred = DeferredEndpointFinalizers::default();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: target has no concurrent users/GC, copied values stay private,
        // and neither the incomplete result nor its native payload is published.
        let transaction = unsafe { target.copy_transaction(&mut deferred) };
        let _session = target.clone_session();
        if !panic_after_copy {
            target.fail_copy_after(2);
        }
        let result = graph.clone_in(target);
        if panic_after_copy {
            assert!(result.is_ok());
            panic!("injected unwind before publication");
        }
        assert!(result.is_err());
        drop(transaction);
    }));
    target.clear_copy_failure();
    assert_eq!(outcome.is_err(), panic_after_copy);
    assert_eq!(target.tracked_endpoint_allocations(), before);
    // The rollback batch owns the provisional wrapper until lock-free draining.
    drop(deferred);
    let ObjectType::ChannelSender(sender) = sender.untag() else { unreachable!() };
    sender.close();
    assert_eq!(manager.counts_for_test(id), Some((0, 1)));
    assert!(matches!(receiver.try_recv(&source), Err(RecvError::Closed)));
    receiver.close();
    assert_eq!(manager.counts_for_test(id), None);
    // An error/unwind must not leave an active transaction or poisoned cache.
    let mut retry_deferred = DeferredEndpointFinalizers::default();
    // SAFETY: same owner block, no GC, and no copied value published before commit.
    let retry = unsafe { target.copy_transaction(&mut retry_deferred) };
    let _retry_session = target.clone_session();
    assert!(graph.clone_in(target).is_ok());
    retry.commit();
    // Both source and target remain alive until after immediate-closure checks.
}

#[test]
fn partial_endpoint_copy_rolls_back_owner_target() {
    nested_endpoint_failure(false);
}
#[test]
fn unpublished_endpoint_copy_unwind_rolls_back_owner_target() {
    nested_endpoint_failure(true);
}

#[test]
fn failed_queue_and_receive_copies_do_not_keep_payload_endpoint_alive() {
    for fail_send in [true, false] {
        let manager = ChannelManager::isolated_for_test();
        let (_, carrier_sender, carrier_receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let (payload_id, payload_sender, payload_receiver) =
            make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let source = Context::new(&roots);
        let payload = source.add(payload_sender);
        let graph = source.add(vec![payload, source.add(42)]);
        let before = source.block.tracked_endpoint_allocations();
        if fail_send {
            manager.fail_queue_copy_after(2);
            assert!(matches!(
                carrier_sender.try_send(graph),
                Err(crate::channel_manager::SendError::CopyFailed)
            ));
            manager.clear_queue_copy_failure();
        } else {
            carrier_sender.try_send(graph).unwrap();
            source.block.fail_copy_after(2);
            assert!(matches!(carrier_receiver.try_recv(&source), Err(RecvError::CopyFailed)));
            source.block.clear_copy_failure();
            assert_eq!(source.block.tracked_endpoint_allocations(), before);
        }
        // Removed/failed queue roots no longer retain their endpoint family.
        manager.collect_for_test();
        let ObjectType::ChannelSender(payload) = payload.untag() else { unreachable!() };
        payload.close();
        assert_eq!(manager.counts_for_test(payload_id), Some((0, 1)));
        assert!(matches!(payload_receiver.try_recv(&source), Err(RecvError::Closed)));
        payload_receiver.close();
        carrier_sender.close();
        carrier_receiver.close();
    }
}

#[test]
fn rejected_constant_function_assignment_retires_copied_endpoint_immediately() {
    use crate::core::{
        env::{INTERNED_SYMBOLS, intern, sym},
        object::{Function, NIL},
    };
    use rune_core::macros::list;
    let manager = ChannelManager::isolated_for_test();
    let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
    let roots = RootSet::default();
    let source = Context::new(&roots);
    sym::init_symbols();
    let sender = source.add(sender);
    let function: Function = list!(sym::LAMBDA, NIL, sender; &source).try_into().unwrap();
    let constant = intern("nil", &source);
    let mut deferred = DeferredEndpointFinalizers::default();
    {
        let map = INTERNED_SYMBOLS.lock().unwrap();
        let before = map.global_block().tracked_endpoint_allocations();
        assert!(map.set_func(constant, function, &mut deferred).is_err());
        assert_eq!(map.global_block().tracked_endpoint_allocations(), before);
    }
    drop(deferred); // Outside the symbol mutex: native cleanup may acquire locks.
    let ObjectType::ChannelSender(sender) = sender.untag() else { unreachable!() };
    sender.close();
    assert_eq!(manager.counts_for_test(id), Some((0, 1)));
    assert!(matches!(receiver.try_recv(&source), Err(RecvError::Closed)));
    receiver.close();
}

#[test]
fn failed_global_hash_value_copy_does_not_publish_key_or_retain_its_endpoint() {
    use crate::core::{
        env::INTERNED_SYMBOLS,
        object::{HashTable, IntoObject, LispHashTable, WithLifetime},
    };
    let manager = ChannelManager::isolated_for_test();
    let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
    let roots = RootSet::default();
    let source = Context::new(&roots);
    let sender = source.add(sender);
    let table: crate::core::object::Gc<&'static LispHashTable> = {
        let map = INTERNED_SYMBOLS.lock().unwrap();
        let table = HashTable::default().into_obj(map.global_block());
        // SAFETY: this empty table is allocated in the process-lifetime global
        // arena. No local values enter it except via the audited insert copy.
        unsafe { table.with_lifetime() }
    };
    let table = table.untag();
    // First the endpoint key is copied, then the injected value failure occurs.
    assert!(table.insert_with_copy_failure(sender, source.add(42), 1).is_err());
    assert!(table.get(sender).is_none());
    let ObjectType::ChannelSender(sender) = sender.untag() else { unreachable!() };
    sender.close();
    assert_eq!(manager.counts_for_test(id), Some((0, 1)));
    assert!(matches!(receiver.try_recv(&source), Err(RecvError::Closed)));
    receiver.close();
}
