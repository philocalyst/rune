use rune_display::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
fn text(s: &str) -> InputEvent {
    InputEvent::TextCommit(s.into())
}
#[test]
fn fifo_retries_without_loss_and_returns_overflow_ownership() {
    let (tx, rx) = mpsc::sync_channel(1);
    let sender = InputSender::new(tx, Arc::new(AtomicBool::new(false)));
    let mut queue = InputBacklog::new(sender, 2);
    for s in ["a", "b", "c"] {
        queue.submit(text(s)).unwrap();
    }
    assert_eq!(queue.submit(text("d")), Err(text("d")));
    assert_eq!(queue.pending_len(), 2);
    assert!(queue.flush().is_err());
    assert_eq!(rx.recv().unwrap(), text("a"));
    assert!(queue.flush().is_err());
    assert_eq!(rx.recv().unwrap(), text("b"));
    queue.flush().unwrap();
    assert_eq!(rx.recv().unwrap(), text("c"));
    assert_eq!(queue.pending_len(), 0);
}
#[test]
fn disconnect_keeps_accepted_events_recoverable_and_quit_is_out_of_band() {
    let (tx, rx) = mpsc::sync_channel(0);
    let quit = Arc::new(AtomicBool::new(false));
    let mut queue = InputBacklog::new(InputSender::new(tx, quit.clone()), 1);
    queue.submit(text("a")).unwrap();
    drop(rx);
    assert!(matches!(queue.flush(), Err(mpsc::TrySendError::Disconnected(()))));
    assert!(
        queue
            .submit(InputEvent::Key {
                key: "g".into(),
                text: None,
                control: true,
                meta: false,
                shift: false
            })
            .is_err()
    );
    assert!(quit.load(Ordering::Acquire));
    assert_eq!(queue.drain_pending().collect::<Vec<_>>(), vec![text("a")]);
}
#[test]
fn geometry_deduplicates_only_after_ack_and_rejects_invalid_values() {
    let mut tracker = ResizeTracker::default();
    let event = tracker.pending(800., 600., 2.).unwrap();
    assert!(tracker.pending(800., 600., 2.).is_some());
    tracker.acknowledge(&event);
    assert!(tracker.pending(800., 600., 2.).is_none());
    assert!(tracker.pending(800., 600., 1.).is_some());
    for x in [0., -1., f32::NAN, f32::INFINITY] {
        assert!(tracker.pending(x, 600., 1.).is_none());
        assert!(tracker.pending(800., 600., x).is_none());
    }
}
