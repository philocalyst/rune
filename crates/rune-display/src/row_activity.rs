//! Coalesced executor-neutral activity, not queued messages or paint authority.
use std::{future::Future, pin::Pin, sync::Mutex, task::{Context, Poll, Waker}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowActivity { Ready, Disconnected }

struct Waiter {
    id: u64,
    waker: Waker,
}

#[derive(Default)]
struct State {
    ready: bool,
    closed: bool,
    last_waiter_id: u64,
    waiter: Option<Waiter>,
}

#[derive(Default)]
pub(crate) struct Activity(Mutex<State>);

impl Activity {
    pub(crate) fn signal(&self) {
        let wake = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            if state.closed { return; }
            state.ready = true;
            state.waiter.take()
        };
        if let Some(waiter) = wake { waiter.waker.wake(); }
    }

    pub(crate) fn close(&self) {
        let wake = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            state.closed = true;
            state.ready = false;
            state.waiter.take()
        };
        if let Some(waiter) = wake { waiter.waker.wake(); }
    }

    pub(crate) fn wait(&self) -> RowActivityWait<'_> {
        RowActivityWait { activity: self, waiter_id: None }
    }
}

/// Dropping the future removes only its own registration, not activity/payloads.
pub struct RowActivityWait<'a> {
    activity: &'a Activity,
    waiter_id: Option<u64>,
}

impl Future for RowActivityWait<'_> {
    type Output = RowActivity;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Waker clone/drop/wake may execute external code. Keep all outside mutex.
        let mut candidate = Some(context.waker().clone());
        let (result, replaced) = {
            let mut state = this.activity.0.lock().unwrap_or_else(|error| error.into_inner());
            if state.closed {
                (Poll::Ready(RowActivity::Disconnected), None)
            } else if state.ready {
                state.ready = false;
                (Poll::Ready(RowActivity::Ready), state.waiter.take())
            } else {
                let id = match this.waiter_id {
                    Some(id) => Some(id),
                    None => state.last_waiter_id.checked_add(1),
                };
                if let Some(id) = id {
                    if this.waiter_id.is_none() {
                        state.last_waiter_id = id;
                        this.waiter_id = Some(id);
                    }
                    let waiter = Waiter { id, waker: candidate.take().expect("candidate present") };
                    (Poll::Pending, state.waiter.replace(waiter))
                } else {
                    state.closed = true; // Never recycle a registration namespace.
                    (Poll::Ready(RowActivity::Disconnected), state.waiter.take())
                }
            }
        };
        drop(replaced);
        drop(candidate);
        result
    }
}

impl Drop for RowActivityWait<'_> {
    fn drop(&mut self) {
        let Some(id) = self.waiter_id else { return; };
        let removed = {
            let mut state = self.activity.0.lock().unwrap_or_else(|error| error.into_inner());
            if state.waiter.as_ref().is_some_and(|waiter| waiter.id == id) {
                state.waiter.take()
            } else {
                None
            }
        };
        drop(removed); // A stale future must not remove a newer registration.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Weak, atomic::{AtomicUsize, Ordering}};
    use std::task::Wake;

    #[derive(Default)]
    struct Count(AtomicUsize);
    impl Wake for Count {
        fn wake(self: Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); }
        fn wake_by_ref(self: &Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); }
    }
    fn poll(wait: &mut RowActivityWait<'_>, waker: &Waker) -> Poll<RowActivity> {
        Pin::new(wait).poll(&mut Context::from_waker(waker))
    }

    #[test]
    fn signal_before_registration_and_after_registration_coalesce() {
        let activity = Activity::default();
        let count = Arc::new(Count::default());
        let waker = Waker::from(count.clone());
        let mut wait = activity.wait();
        activity.signal();
        assert_eq!(poll(&mut wait, &waker), Poll::Ready(RowActivity::Ready));
        assert_eq!(poll(&mut wait, &waker), Poll::Pending);
        activity.signal();
        activity.signal();
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert_eq!(poll(&mut wait, &waker), Poll::Ready(RowActivity::Ready));
        assert_eq!(poll(&mut wait, &waker), Poll::Pending);
        activity.close();
        activity.close();
        assert_eq!(count.0.load(Ordering::SeqCst), 2);
        assert_eq!(poll(&mut wait, &waker), Poll::Ready(RowActivity::Disconnected));
    }

    #[test]
    fn canceled_wait_releases_registered_executor_without_consuming_activity() {
        let activity = Activity::default();
        let executor = Arc::new(Count::default());
        let weak = Arc::downgrade(&executor);
        let waker = Waker::from(executor);
        let mut wait = activity.wait();
        assert_eq!(poll(&mut wait, &waker), Poll::Pending);
        drop(waker);
        assert!(weak.upgrade().is_some());
        drop(wait);
        assert!(weak.upgrade().is_none());
        activity.signal();
        let waker = Waker::from(Arc::new(Count::default()));
        assert_eq!(poll(&mut activity.wait(), &waker), Poll::Ready(RowActivity::Ready));
    }

    #[test]
    fn old_future_drop_does_not_remove_newer_waiter() {
        let activity = Activity::default();
        let first_count = Arc::new(Count::default());
        let first_waker = Waker::from(first_count.clone());
        let second_count = Arc::new(Count::default());
        let second_waker = Waker::from(second_count.clone());
        let mut first = activity.wait();
        let mut second = activity.wait();
        assert_eq!(poll(&mut first, &first_waker), Poll::Pending);
        assert_eq!(poll(&mut second, &second_waker), Poll::Pending);
        drop(first);
        activity.signal();
        assert_eq!(first_count.0.load(Ordering::SeqCst), 0);
        assert_eq!(second_count.0.load(Ordering::SeqCst), 1);
        assert_eq!(poll(&mut second, &second_waker), Poll::Ready(RowActivity::Ready));
    }

    struct ReentrantDrop(Weak<Activity>);
    impl Wake for ReentrantDrop { fn wake(self: Arc<Self>) {} }
    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            if let Some(activity) = self.0.upgrade() { activity.signal(); }
        }
    }

    #[test]
    fn cancellation_drops_reentrant_waker_outside_mutex() {
        let activity = Arc::new(Activity::default());
        let waker = Waker::from(Arc::new(ReentrantDrop(Arc::downgrade(&activity))));
        let mut wait = activity.wait();
        assert_eq!(poll(&mut wait, &waker), Poll::Pending);
        drop(waker);
        drop(wait); // Last Waker Drop calls signal and must not deadlock.
        let waker = Waker::from(Arc::new(Count::default()));
        assert_eq!(poll(&mut activity.wait(), &waker), Poll::Ready(RowActivity::Ready));
    }

    #[test]
    fn concurrent_registration_and_signal_has_no_lost_activity() {
        for _ in 0..64 {
            let activity = Arc::new(Activity::default());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let producer = activity.clone();
            let producer_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                producer_barrier.wait();
                producer.signal();
            });
            let count = Arc::new(Count::default());
            let waker = Waker::from(count.clone());
            let mut wait = activity.wait();
            barrier.wait();
            let initial = poll(&mut wait, &waker);
            worker.join().unwrap();
            if initial == Poll::Pending {
                assert!(count.0.load(Ordering::SeqCst) > 0);
                assert_eq!(poll(&mut wait, &waker), Poll::Ready(RowActivity::Ready));
            } else {
                assert_eq!(initial, Poll::Ready(RowActivity::Ready));
            }
        }
    }

    #[test]
    fn exhausted_registration_counter_closes_without_reuse() {
        let activity = Activity::default();
        activity.0.lock().unwrap().last_waiter_id = u64::MAX;
        let waker = Waker::from(Arc::new(Count::default()));
        assert_eq!(poll(&mut activity.wait(), &waker), Poll::Ready(RowActivity::Disconnected));
        assert!(activity.0.lock().unwrap().closed);
    }
}
