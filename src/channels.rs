//! Channel operations for CSP-style concurrency.

use crate::core::{
    cons::Cons,
    gc::Context,
    object::{
        ChannelReceiver, ChannelSender, Gc, IntoObject, NIL, Object, Symbol, make_channel_pair,
    },
};
use rune_macros::defun;
use std::time::Duration;

#[defun]
fn make_channel<'ob>(capacity: Option<i64>, cx: &'ob Context) -> Object<'ob> {
    let cap = capacity.unwrap_or(1).max(1) as usize;
    let (sender, receiver) = make_channel_pair(cap);
    // Return (sender . receiver) as a cons pair
    Cons::new(sender.into_obj(&cx.block), receiver.into_obj(&cx.block), cx).into()
}

#[defun]
fn channel_send<'ob>(sender: Gc<&ChannelSender>, obj: Object<'ob>) -> Object<'ob> {
    match sender.send(obj) {
        Ok(()) => NIL,
        Err(e) => Symbol::from(e).into(),
    }
}

#[defun]
fn channel_recv<'ob>(receiver: Gc<&ChannelReceiver>, cx: &'ob Context) -> Object<'ob> {
    match receiver.recv(cx) {
        Ok(obj) => obj,
        Err(e) => Symbol::from(e).into(),
    }
}

#[defun]
fn channel_try_send<'ob>(sender: Gc<&ChannelSender>, obj: Object<'ob>) -> Object<'ob> {
    match sender.try_send(obj) {
        Ok(()) => NIL,
        Err(e) => Symbol::from(e).into(),
    }
}

#[defun]
fn channel_try_recv<'ob>(receiver: Gc<&ChannelReceiver>, cx: &'ob Context) -> Object<'ob> {
    match receiver.try_recv(cx) {
        Ok(obj) => obj,
        Err(e) => Symbol::from(e).into(),
    }
}

/// Rune-specific timeout policy: finite negatives are immediate; invalid native
/// durations fail before any endpoint lease, graph copy or queue operation.
fn channel_timeout(seconds: f64) -> anyhow::Result<Duration> {
    anyhow::ensure!(seconds.is_finite(), "Channel timeout must be finite and representable");
    Duration::try_from_secs_f64(seconds.max(0.0))
        .map_err(|_| anyhow::anyhow!("Channel timeout must be finite and representable"))
}

#[defun]
fn channel_send_timeout<'ob>(
    sender: Gc<&ChannelSender>,
    obj: Object<'ob>,
    timeout_secs: f64,
) -> anyhow::Result<Object<'ob>> {
    let timeout = channel_timeout(timeout_secs)?;
    Ok(match sender.send_timeout(obj, timeout) {
        Ok(()) => NIL,
        Err(e) => Symbol::from(e).into(),
    })
}

#[defun]
fn channel_recv_timeout<'ob>(
    receiver: Gc<&ChannelReceiver>,
    timeout_secs: f64,
    cx: &'ob Context,
) -> anyhow::Result<Object<'ob>> {
    let timeout = channel_timeout(timeout_secs)?;
    Ok(match receiver.recv_timeout(cx, timeout) {
        Ok(obj) => obj,
        Err(e) => Symbol::from(e).into(),
    })
}

/// Close the sender side of a channel.
///
/// After closing, no more values can be sent through this sender.
/// Any receivers waiting on this channel will be woken up.
/// If all senders are closed or dropped, receivers will get 'channel-closed.
#[defun]
fn channel_sender_close<'ob>(sender: Gc<&ChannelSender>) -> Object<'ob> {
    sender.close();
    NIL
}

/// Close the receiver side of a channel.
///
/// After closing, no more values can be received from this receiver.
/// This is useful for "moving" a receiver to another thread by cloning
/// it into a new context and then closing the original.
///
/// This is idempotent - calling it multiple times has no additional effect.
#[defun]
fn channel_receiver_close<'ob>(receiver: Gc<&ChannelReceiver>) -> Object<'ob> {
    receiver.close();
    NIL
}


#[cfg(test)]
mod timeout_boundary_tests {
    use super::*;
    use crate::{channel_manager::{ChannelManager, RecvError}, core::{gc::RootSet, object::make_channel_pair_for_test}, root};

    #[test]
    fn finite_timeout_policy_preserves_negative_zero_and_fractional_seconds() {
        for seconds in [-f64::MAX, -1.0, -0.0, 0.0] {
            assert_eq!(channel_timeout(seconds).unwrap(), Duration::ZERO);
        }
        assert_eq!(channel_timeout(0.125).unwrap(), Duration::from_millis(125));
        assert_eq!(channel_timeout(1.5).unwrap(), Duration::from_millis(1500));
        for seconds in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, 2.0f64.powi(64)] {
            assert!(channel_timeout(seconds).is_err());
        }
    }

    #[test]
    fn invalid_timeout_never_mutates_queue_or_retires_endpoint_leases() {
        let manager = ChannelManager::isolated_for_test();
        let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let sender = sender.into_obj(&cx.block);
        root!(sender, cx);
        let receiver = receiver.into_obj(&cx.block);
        root!(receiver, cx);
        for seconds in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            sender.bind(cx).try_send(cx.add(7)).unwrap();
            assert!(channel_send_timeout(sender.bind(cx), cx.add(99), seconds).is_err());
            assert!(channel_recv_timeout(receiver.bind(cx), seconds, cx).is_err());
            assert_eq!(manager.counts_for_test(id), Some((1, 1)));
            cx.garbage_collect(true);
            assert_eq!(manager.counts_for_test(id), Some((1, 1)));
            assert_eq!(receiver.bind(cx).try_recv(cx).unwrap(), 7);
            assert_eq!(receiver.bind(cx).try_recv(cx), Err(RecvError::Empty));
            // Both original endpoints are still usable after conversion failure.
            assert!(channel_send_timeout(sender.bind(cx), cx.add(11), -1.0).unwrap().is_nil());
            assert_eq!(channel_recv_timeout(receiver.bind(cx), 0.0, cx).unwrap(), 11);
        }
        sender.bind(cx).close();
        receiver.bind(cx).close();
        cx.garbage_collect(true);
        assert_eq!(manager.counts_for_test(id), None);
    }
    #[test]
    fn invalid_timeout_is_catchable_through_real_lisp_builtin_dispatch() {
        for seconds in ["0.0e+NaN", "1.0e+INF", "-1.0e+INF", "1.0e+300"] {
            let form = format!(r#"(let ((pair (make-channel 1)))
              (channel-try-send (car pair) 7)
              (list
                (condition-case err (channel-send-timeout (car pair) 99 {seconds})
                  (error (car err)))
                (condition-case err (channel-recv-timeout (cdr pair) {seconds})
                  (error (car err)))
                (channel-try-recv (cdr pair))
                (channel-sender-close (car pair))
                (channel-receiver-close (cdr pair))))"#);
            crate::interpreter::assert_lisp(&form, "(error error 7 nil nil)");
        }
    }

}
