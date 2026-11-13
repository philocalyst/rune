//! Multi-threaded elisp support.
//!
//! The `go` function spawns a new thread to evaluate an expression and returns
//! a channel receiver that will eventually contain the result.
//!
//! # Result Format
//!
//! - On success: The receiver contains the evaluation result
//! - On error: The receiver contains `('thread-errored . "error message")`
//!
//! # Usage from Elisp
//!
//! ```elisp
//! ;; Successful evaluation
//! (let ((receiver (go (+ 1 2))))
//!   (channel-recv receiver))  ; Returns 3
//!
//! ;; Error handling with pcase
//! (let ((receiver (go (undefined-function))))
//!   (pcase (channel-recv receiver)
//!     (`(thread-errored . ,msg)
//!      (message "Thread error: %s" msg))
//!     (result
//!      (message "Got result: %s" result))))
//!
//! ;; Discard result (fire and forget)
//! (go (message "Background task"))
//! ```

use crate::core::{
    cons::Cons,
    env::{Env, sym},
    gc::{Context, RootSet},
    object::{IntoObject, Object, make_channel_pair},
};
use rune_core::macros::root;
use rune_macros::defun;
use std::thread;

#[defun]
fn go<'ob>(obj: Object, cx: &'ob Context) -> Object<'ob> {
    go_internal(obj, cx)
}

fn go_internal<'ob>(obj: Object, cx: &'ob Context) -> Object<'ob> {
    // Create a rendezvous channel (capacity 1) for the result
    let (sender, receiver) = make_channel_pair(1);

    let (init_sender, init_receiver) = make_channel_pair(1);
    init_sender.send(obj).expect("channel is immediately used after creation");
    init_sender.close();

    // Spawn thread to evaluate expression
    thread::spawn(move || {
        // A worker owns the ordinary singleton Context on its fresh thread.
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        // Each worker has its own variable bindings, including the same built-in
        // defaults as the main interpreter. Root them before evaluation can GC.
        crate::core::env::init_variables(cx, env);

        // Reconstruct the object from raw
        let obj = init_receiver.recv(cx).expect("go_internal parent thread waited on the send operation in this channel before spawning the thread");
        init_receiver.close();
        root!(obj, cx);

        // Evaluate expression and create result
        let result = match crate::interpreter::eval(obj, None, env, cx) {
            Ok(value) => value,
            Err(err) => {
                // Create ('thread-errored . "error message") cons pair
                let error_msg = cx.add(err.to_string());
                Cons::new(sym::THREAD_ERRORED, error_msg, cx).into()
            }
        };

        // Send result through the channel
        // The result is already in the thread's context (block),
        // and will be double-copied: once into channel's buffer_block,
        // then once into receiver's context when received
        let _ = sender.send(result);
        sender.close();
        cx.garbage_collect(true);
    });

    // Return the receiver to the caller
    receiver.into_obj(&cx.block).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::object::{ChannelReceiver, Gc, ObjectType};

    #[test]
    fn roadmap_m0_go_initializes_scalar_builtin_defaults() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);

        for (variable, expected) in [
            (sym::FILL_COLUMN, cx.add(70)),
            (sym::TAB_WIDTH, cx.add(8)),
            (sym::LEXICAL_BINDING, cx.add(true)),
            (sym::PRINT_LENGTH, cx.add(false)),
        ] {
            let receiver_obj = go_internal(variable.into(), cx);
            let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
            assert_eq!(receiver.recv(cx).unwrap(), expected, "default for {variable}");
        }
    }

    #[test]
    fn roadmap_m0_go_initializes_allocated_builtin_defaults() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let receiver_obj = go_internal(sym::LOAD_PATH.into(), cx);
        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        let result = receiver.recv(cx).unwrap();
        let ObjectType::Cons(path) = result.untag() else {
            panic!("Expected initialized load-path, got {result:?}");
        };
        assert_eq!(path.car(), cx.add(format!("{}/lisp", env!("CARGO_MANIFEST_DIR"))));
        assert_eq!(path.cdr(), crate::core::object::NIL);
    }

    #[test]
    fn roadmap_m0_go_defaults_are_local_to_child_env() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        crate::core::env::init_variables(cx, env);
        env.set_var(sym::FILL_COLUMN, cx.add(91)).unwrap();

        // A worker's assignment must not change either the parent or the next
        // worker's defaults. SETQ/PROGN are special forms, not global subr setup.
        let form = crate::reader::read("(progn (setq fill-column 23) fill-column)", cx).unwrap().0;
        let receiver_obj = go_internal(form, cx);
        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        assert_eq!(receiver.recv(cx).unwrap(), cx.add(23));
        assert_eq!(env.vars.get(sym::FILL_COLUMN).unwrap().bind(cx), cx.add(91));

        let receiver_obj = go_internal(sym::FILL_COLUMN.into(), cx);
        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        assert_eq!(receiver.recv(cx).unwrap(), cx.add(70));
    }

    #[test]
    fn roadmap_m0_go_preserves_error_channel_result() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let missing = crate::core::env::intern("roadmap-m0-unbound-worker-variable", cx);
        let receiver_obj = go_internal(missing.into(), cx);
        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        let result = receiver.recv(cx).unwrap();
        let ObjectType::Cons(error) = result.untag() else {
            panic!("Expected thread error result, got {result:?}");
        };
        assert_eq!(error.car(), sym::THREAD_ERRORED);
        assert!(matches!(error.cdr().untag(), ObjectType::String(_)));
    }

    #[test]
    fn test_go() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let obj = cx.add("test string");
        let receiver_obj = go_internal(obj, cx);

        // Verify it returns a ChannelReceiver
        assert!(matches!(receiver_obj.untag(), ObjectType::ChannelReceiver(_)));

        // Receive and verify we got a result
        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        let result = receiver.recv(cx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_go_eval() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);

        let receivers = [
            go_internal(crate::reader::read("(if nil 1 2 3)", cx).unwrap().0, cx),
            go_internal(crate::reader::read("(progn (defvar foo 1) foo)", cx).unwrap().0, cx),
            go_internal(crate::reader::read("(progn (defvar foo 1) (makunbound 'foo) (let ((fn #'(lambda () (defvar foo 3))) (foo 7)) (funcall fn)) foo)", cx).unwrap().0, cx),
        ];

        for recv_obj in receivers {
            let receiver: Gc<&ChannelReceiver> = recv_obj.try_into().unwrap();
            let result = receiver.recv(cx);
            assert!(result.is_ok(), "Expected successful evaluation");
        }
    }

    #[test]
    fn test_go_message() {
        let roots = &RootSet::default();
        println!("hello main thread");
        let cx = &mut Context::new(roots);
        let obj = crate::reader::read("(message \"hello from thread\")", cx).unwrap().0;
        let receiver_obj = go_internal(obj, cx);

        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        let result = receiver.recv(cx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_go_error() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);

        // This expression should error (undefined function)
        let obj = crate::reader::read("(undefined-function-xyz)", cx).unwrap().0;
        let receiver_obj = go_internal(obj, cx);

        let receiver: Gc<&ChannelReceiver> = receiver_obj.try_into().unwrap();
        let result = receiver.recv(cx).unwrap();

        // Should receive ('thread-errored . "error message") cons pair
        match result.untag() {
            ObjectType::Cons(cons) => {
                let car = cons.car();
                // Verify car is 'thread-errored symbol
                assert!(matches!(car.untag(), ObjectType::Symbol(_)));
                // Verify cdr is a string with error message
                let cdr = cons.cdr();
                assert!(matches!(cdr.untag(), ObjectType::String(_)));
            }
            _ => panic!("Expected cons pair for error result, got: {result:?}"),
        }
    }

    #[test]
    fn test_go_discard_receiver() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);

        // Spawn thread but discard receiver immediately
        let _receiver = go_internal(cx.add("discarded"), cx);
        let _ = _receiver;

        cx.garbage_collect(true);

        // Thread should complete normally even though receiver was dropped
        // Wait briefly to allow thread to attempt send
        std::thread::sleep(std::time::Duration::from_millis(50));

        cx.garbage_collect(true);
        // If we get here without panic, test passes
    }
}
