use crate::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{SyncSender, TrySendError},
};
/// Latest-only display mailbox: publication never waits on a frontend render.
/// Short mutex scope only clones/swaps Arc; no paint or layout while locked.
#[derive(Default, Clone)]
pub struct DisplayMailbox(Arc<Mutex<Option<Arc<FrameDisplay>>>>);
impl DisplayMailbox {
    pub fn publish(&self, frame: Arc<FrameDisplay>) -> Result<(), &'static str> {
        if frame.protocol_version != DISPLAY_PROTOCOL_VERSION {
            return Err("incompatible display protocol");
        }
        let mut slot = self.0.lock().map_err(|_| "display mailbox poisoned")?;
        if slot.as_ref().is_some_and(|old| old.generation >= frame.generation) {
            return Err("stale display generation");
        }
        *slot = Some(frame);
        Ok(())
    }
    pub fn latest(&self) -> Option<Arc<FrameDisplay>> {
        self.0.lock().ok()?.clone()
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Key {
        key: String,
        text: Option<String>,
        control: bool,
        meta: bool,
        shift: bool,
    },
    Pointer {
        point: Point,
        button: u8,
        revision: u64,
    },
    PointerHit {
        hit: Hit,
        button: u8,
    },
    Scroll {
        dx: f32,
        dy: f32,
    },
    Resize {
        width: f32,
        height: f32,
        scale: f32,
    },
    TextCommit(String),
}
/// Bounded nonblocking adapter, suitable for GPUI event callbacks. A full queue
/// is explicitly reported; committed text/key events must be retried by caller.
#[derive(Clone)]
pub struct InputSender {
    sender: SyncSender<InputEvent>,
    quit: Arc<AtomicBool>,
}
impl InputSender {
    pub fn new(sender: SyncSender<InputEvent>, quit: Arc<AtomicBool>) -> Self {
        Self { sender, quit }
    }
    fn signal_quit(&self, event: &InputEvent) {
        if matches!(event,InputEvent::Key{key,control:true,..} if key=="g") {
            self.quit.store(true, Ordering::Release);
        }
    }
    pub fn send(&self, event: InputEvent) -> Result<(), TrySendError<InputEvent>> {
        self.signal_quit(&event);
        self.sender.try_send(event)
    }
}

/// Finite UI-side retry FIFO. Accepted events are retained on downstream Full
/// or Disconnected, in order. Saturation returns the new event to the host;
/// there is no unbounded overflow allocation, implicit drop or polling thread.
pub struct InputBacklog {
    sender: InputSender,
    pending: std::collections::VecDeque<InputEvent>,
    capacity: usize,
}
impl InputBacklog {
    pub fn new(sender: InputSender, capacity: usize) -> Self {
        Self { sender, pending: Default::default(), capacity }
    }
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    /// Retry on the UI thread after an explicit consumer-capacity wake.
    pub fn flush(&mut self) -> Result<(), TrySendError<()>> {
        while let Some(event) = self.pending.pop_front() {
            match self.sender.send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    self.pending.push_front(event);
                    return Err(TrySendError::Full(()));
                }
                Err(TrySendError::Disconnected(event)) => {
                    self.pending.push_front(event);
                    return Err(TrySendError::Disconnected(()));
                }
            }
        }
        Ok(())
    }
    pub fn submit(&mut self, event: InputEvent) -> Result<(), InputEvent> {
        // Quit must remain observable even when the retry FIFO is saturated.
        self.sender.signal_quit(&event);
        let _ = self.flush();
        if self.pending.is_empty() {
            match self.sender.send(event) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(event) | TrySendError::Disconnected(event)) => {
                    if self.capacity == 0 {
                        return Err(event);
                    }
                    self.pending.push_back(event);
                    return Ok(());
                }
            }
        }
        if self.pending.len() == self.capacity {
            return Err(event);
        }
        self.pending.push_back(event);
        Ok(())
    }
    /// Recover accepted-but-undelivered events, for disconnect/shutdown handoff.
    pub fn drain_pending(&mut self) -> impl Iterator<Item = InputEvent> + '_ {
        self.pending.drain(..)
    }
}

/// Suppress duplicate geometry events without acknowledging a rejected update.
#[derive(Default)]
pub struct ResizeTracker(Option<(f32, f32, f32)>);
impl ResizeTracker {
    pub fn pending(&self, width: f32, height: f32, scale: f32) -> Option<InputEvent> {
        if ![width, height, scale].iter().all(|v| v.is_finite() && *v > 0.0)
            || self.0 == Some((width, height, scale))
        {
            return None;
        }
        Some(InputEvent::Resize { width, height, scale })
    }
    pub fn acknowledge(&mut self, event: &InputEvent) {
        if let InputEvent::Resize { width, height, scale } = *event {
            if [width, height, scale].iter().all(|v| v.is_finite() && *v > 0.0) {
                self.0 = Some((width, height, scale));
            }
        }
    }
}
