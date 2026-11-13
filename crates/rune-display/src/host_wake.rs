//! Executor-neutral worker → UI wake bridge. One latest frame and one capacity
//! bit, not an accumulating frame/event queue; idle consumers register a Waker.
use crate::{DISPLAY_PROTOCOL_VERSION, FrameDisplay};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct State {
    frame: Option<Arc<FrameDisplay>>,
    generation: Option<u64>,
    input_capacity: bool,
    publishers: usize,
    receiver_alive: bool,
    waker: Option<Waker>,
}
/// Latest-only frame publication and coalesced downstream-input capacity wakes.
pub struct HostPublisher(Arc<Mutex<State>>);
pub struct HostUpdates(Arc<Mutex<State>>);
#[derive(Default)]
pub struct HostUpdate {
    pub frame: Option<Arc<FrameDisplay>>,
    pub input_capacity: bool,
}
pub fn host_updates() -> (HostPublisher, HostUpdates) {
    let state =
        Arc::new(Mutex::new(State { publishers: 1, receiver_alive: true, ..Default::default() }));
    (HostPublisher(state.clone()), HostUpdates(state))
}
impl Clone for HostPublisher {
    fn clone(&self) -> Self {
        self.0.lock().expect("host bridge poisoned").publishers += 1;
        Self(self.0.clone())
    }
}
impl HostPublisher {
    pub fn publish(&self, frame: Arc<FrameDisplay>) -> Result<(), &'static str> {
        if frame.protocol_version != DISPLAY_PROTOCOL_VERSION {
            return Err("incompatible display protocol");
        }
        let wake = {
            let mut state = self.0.lock().map_err(|_| "host bridge poisoned")?;
            if !state.receiver_alive {
                return Err("host UI disconnected");
            }
            if state.generation.is_some_and(|old| old >= frame.generation) {
                return Err("stale display generation");
            }
            state.generation = Some(frame.generation);
            state.frame = Some(frame);
            state.waker.take()
        };
        // Never invoke executor or user code while holding the state mutex.
        if let Some(wake) = wake {
            wake.wake();
        }
        Ok(())
    }
    /// Call after removing input from the bounded editor channel, including
    /// events that produce no frame. A frame-only wake can deadlock key retries.
    pub fn input_capacity_available(&self) -> Result<(), &'static str> {
        let wake = {
            let mut state = self.0.lock().map_err(|_| "host bridge poisoned")?;
            if !state.receiver_alive {
                return Err("host UI disconnected");
            }
            state.input_capacity = true;
            state.waker.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        Ok(())
    }
}
impl Drop for HostPublisher {
    fn drop(&mut self) {
        let wake = if let Ok(mut state) = self.0.lock() {
            state.publishers -= 1;
            if state.publishers == 0 { state.waker.take() } else { None }
        } else {
            None
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}
impl HostUpdates {
    /// Cancellation-safe: dropping this future does not consume an update.
    /// Drop the HostUpdates itself to disconnect and reject future publication.
    pub fn next(&mut self) -> NextHostUpdate<'_> {
        NextHostUpdate(self)
    }
}
pub struct NextHostUpdate<'a>(&'a mut HostUpdates);
impl Future for NextHostUpdate<'_> {
    type Output = Option<HostUpdate>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut state) = self.0.0.lock() else {
            return Poll::Ready(None);
        };
        if state.frame.is_some() || state.input_capacity {
            let update = HostUpdate {
                frame: state.frame.take(),
                input_capacity: std::mem::take(&mut state.input_capacity),
            };
            return Poll::Ready(Some(update));
        }
        if state.publishers == 0 {
            return Poll::Ready(None);
        }
        if !state.waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
            state.waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}
impl Drop for HostUpdates {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.receiver_alive = false;
            state.frame = None;
            state.waker = None;
        }
    }
}
