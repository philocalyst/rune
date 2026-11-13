use rune_display::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
#[derive(Default)]
struct Counter(AtomicUsize);
impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
fn frame(generation: u64) -> Arc<FrameDisplay> {
    Arc::new(FrameDisplay {
        protocol_version: DISPLAY_PROTOCOL_VERSION,
        generation,
        bounds: Rect { x: 0., y: 0., width: 80., height: 60. },
        background: Face::default().background,
        windows: vec![],
    })
}
#[test]
fn coalesces_frames_and_capacity_without_losing_next_wake() {
    let (publisher, mut updates) = host_updates();
    let counter = Arc::new(Counter::default());
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut updates.next()).poll(&mut cx).is_pending());
    publisher.publish(frame(1)).unwrap();
    publisher.publish(frame(2)).unwrap();
    publisher.input_capacity_available().unwrap();
    assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    let Poll::Ready(Some(update)) = Pin::new(&mut updates.next()).poll(&mut cx) else {
        panic!()
    };
    assert_eq!(update.frame.unwrap().generation, 2);
    assert!(update.input_capacity);
    assert!(publisher.publish(frame(2)).is_err());
    assert!(Pin::new(&mut updates.next()).poll(&mut cx).is_pending());
    publisher.input_capacity_available().unwrap();
    assert_eq!(counter.0.load(Ordering::Relaxed), 2);
    let Poll::Ready(Some(update)) = Pin::new(&mut updates.next()).poll(&mut cx) else {
        panic!()
    };
    assert!(update.frame.is_none());
    assert!(update.input_capacity);
}
#[test]
fn last_publisher_close_wakes_and_drains_pending_then_ends() {
    let (publisher, mut updates) = host_updates();
    let clone = publisher.clone();
    let counter = Arc::new(Counter::default());
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut updates.next()).poll(&mut cx).is_pending());
    drop(publisher);
    assert_eq!(counter.0.load(Ordering::Relaxed), 0);
    clone.publish(frame(8)).unwrap();
    drop(clone);
    assert!(matches!(Pin::new(&mut updates.next()).poll(&mut cx), Poll::Ready(Some(_))));
    assert!(matches!(Pin::new(&mut updates.next()).poll(&mut cx), Poll::Ready(None)));
}
#[test]
fn cancelled_wait_replaces_waker_and_ui_drop_rejects_publication() {
    let (publisher, mut updates) = host_updates();
    let first = Arc::new(Counter::default());
    let second = Arc::new(Counter::default());
    for counter in [&first, &second] {
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut updates.next()).poll(&mut cx).is_pending());
    }
    let mut incompatible = (*frame(1)).clone();
    incompatible.protocol_version += 1;
    assert!(publisher.publish(Arc::new(incompatible)).is_err());
    assert_eq!(second.0.load(Ordering::Relaxed), 0);
    publisher.publish(frame(1)).unwrap();
    assert_eq!(first.0.load(Ordering::Relaxed), 0);
    assert_eq!(second.0.load(Ordering::Relaxed), 1);
    drop(updates);
    assert!(publisher.publish(frame(2)).is_err());
    assert!(publisher.input_capacity_available().is_err());
}

#[test]
fn threaded_publication_never_loses_final_generation_or_close_wake() {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let (publisher, mut updates) = host_updates();
    let worker = std::thread::spawn(move || {
        for generation in 1..=1000 {
            publisher.publish(frame(generation)).unwrap();
        }
    });
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut last = 0;
    loop {
        match Pin::new(&mut updates.next()).poll(&mut cx) {
            Poll::Ready(Some(update)) => {
                let generation = update.frame.unwrap().generation;
                assert!(generation > last);
                last = generation;
            }
            Poll::Ready(None) => break,
            Poll::Pending => {
                assert!(std::time::Instant::now() < deadline, "lost host wake");
                std::thread::park_timeout(std::time::Duration::from_millis(10));
            }
        }
    }
    worker.join().unwrap();
    assert_eq!(last, 1000);
}
