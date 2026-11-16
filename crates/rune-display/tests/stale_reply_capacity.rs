//! A stale reply must restore capacity without consuming a fresh pending request.
use rune_display::retained_row_handoff::*;
use rune_display::retained_row_transaction::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::{AtomicUsize, Ordering}},
    task::{Context, Poll, Wake, Waker},
};

fn scope() -> RetainedRowScope {
    RetainedRowScope {
        sequence: 1, window: 2, buffer: 3, text_epoch: 4, source_start_scalar: 0,
        style_revision: 5, viewport_revision: 6, font_bank_generation: 7,
        shaping_policy_revision: 8,
    }
}

#[derive(Default)]
struct WakeCount(AtomicUsize);
impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn stale_reply_error_restores_capacity_wake_without_consuming_fresh_pending() {
    let (mut owner, mut ui) = row_preparation_handoff().unwrap();
    let old = owner.prepare(scope(), "old").unwrap();
    ui.next_request().unwrap().unwrap();
    owner.invalidate();
    let fresh = owner.prepare(scope(), "fresh").unwrap();
    let request = ui.next_request().unwrap().unwrap();
    assert!(request.is_live());

    ui.reply(RetainedRowReply { key: old, ui_token: 1 }).unwrap();
    let retry = ui.reply(RetainedRowReply { key: fresh, ui_token: 2 }).unwrap_err();
    assert_eq!(retry.error, RowHandoffError::Saturated);
    assert_eq!(retry.reply.key, fresh);

    // Consume preceding coalesced activity without consuming any mailbox data.
    // The fixture has a finite backlog and no concurrent producer in this phase.
    while Pin::new(&mut ui.wait_for_activity())
        .poll(&mut Context::from_waker(Waker::noop())) == Poll::Ready(RowActivity::Ready)
    {}

    let count = Arc::new(WakeCount::default());
    let waker = Waker::from(count.clone());
    {
        // Retain this actual registration until after the failing owner dequeue.
        let mut wait = ui.wait_for_activity();
        assert_eq!(Pin::new(&mut wait).poll(&mut Context::from_waker(&waker)), Poll::Pending);
        assert!(matches!(owner.poll(scope(), "fresh"),
            Err(RowHandoffError::Transaction(RetainedRowError::UnknownRequest))));
        assert!(request.is_live());
        assert!(count.0.load(Ordering::SeqCst) > 0);
        assert_eq!(Pin::new(&mut wait).poll(&mut Context::from_waker(&waker)),
            Poll::Ready(RowActivity::Ready));
    }

    // Retry only the exact reply whose ownership was returned by the failed send.
    ui.reply(retry.reply).unwrap();
    let token = owner.poll(scope(), "fresh").unwrap().unwrap();
    assert_eq!(token.key(), fresh);
    assert_eq!(token.ui_token(), 2);
    assert!(token.is_live());
    assert_eq!(ui.next_acceptance().unwrap().unwrap().key(), fresh);
    assert!(owner.poll(scope(), "fresh").unwrap().is_none());
}
