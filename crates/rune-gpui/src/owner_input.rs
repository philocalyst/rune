//! UI-local ordered keyboard/composition FIFO. No Context, live document or
//! platform InputHandler is held here. All callbacks run outside RefCell borrows.
use crate::{
    editor_transport::{CompositionAck, NativeEditorReplies, NativeEditorSender, OwnerEvent},
    input_policy::{InputPolicy, InputReadiness, RejectionReason},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TrySendError,
    },
};
#[derive(Debug)]
pub struct OwnerInputRejection {
    pub event: OwnerEvent,
    pub reason: RejectionReason,
}
pub type RejectedOwnerInput = Rc<dyn Fn(OwnerInputRejection)>;
pub type CompositionOutcome = Rc<dyn Fn(CompositionAck)>;
pub(crate) struct OwnerInput {
    sender: NativeEditorSender,
    pending: VecDeque<OwnerEvent>,
    policy: InputPolicy,
    capacity: usize,
    quit: Arc<AtomicBool>,
    rejected: RejectedOwnerInput,
    replies: NativeEditorReplies,
    outcome: CompositionOutcome,
}
impl OwnerInput {
    pub(crate) fn new(
        sender: NativeEditorSender,
        replies: NativeEditorReplies,
        quit: Arc<AtomicBool>,
        rejected: RejectedOwnerInput,
        outcome: CompositionOutcome,
    ) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            sender,
            pending: VecDeque::new(),
            policy: InputPolicy::default(),
            capacity: 256,
            quit,
            rejected,
            replies,
            outcome,
        }))
    }
    fn signal_quit(&self, e: &OwnerEvent) {
        if matches!(e,OwnerEvent::Input(rune_display::InputEvent::Key{key,control:true,..}) if key=="g")
        {
            self.quit.store(true, Ordering::Release);
        }
    }
    fn flush(&mut self) -> Result<(), TrySendError<()>> {
        while let Some(e) = self.pending.pop_front() {
            match self.sender.try_send(e) {
                Ok(()) => {}
                Err(TrySendError::Full(e)) => {
                    self.pending.push_front(e);
                    return Err(TrySendError::Full(()));
                }
                Err(TrySendError::Disconnected(e)) => {
                    self.pending.push_front(e);
                    return Err(TrySendError::Disconnected(()));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn submit(input: &Rc<RefCell<Self>>, event: OwnerEvent) -> bool {
        let (callback, rejected, accepted) = {
            let mut i = input.borrow_mut();
            i.signal_quit(&event);
            let callback = i.rejected.clone();
            if let Some(reason) = i.policy.rejection() {
                (callback, vec![OwnerInputRejection { event, reason }], false)
            } else if matches!(i.flush(), Err(TrySendError::Disconnected(()))) {
                i.policy.disconnect();
                let mut rejected: Vec<_> = i
                    .pending
                    .drain(..)
                    .map(|event| OwnerInputRejection {
                        event,
                        reason: RejectionReason::HostDisconnected,
                    })
                    .collect();
                rejected
                    .push(OwnerInputRejection { event, reason: RejectionReason::HostDisconnected });
                (callback, rejected, false)
            } else {
                let undelivered = if i.pending.is_empty() {
                    match i.sender.try_send(event) {
                        Ok(()) => None,
                        Err(TrySendError::Full(e)) => Some(e),
                        Err(TrySendError::Disconnected(e)) => {
                            i.policy.disconnect();
                            Some(e)
                        }
                    }
                } else {
                    Some(event)
                };
                match undelivered {
                    None => (callback, vec![], true),
                    Some(event) if i.policy.state() == InputReadiness::Disconnected => (
                        callback,
                        vec![OwnerInputRejection {
                            event,
                            reason: RejectionReason::HostDisconnected,
                        }],
                        false,
                    ),
                    Some(event) if i.pending.len() < i.capacity => {
                        i.pending.push_back(event);
                        (callback, vec![], true)
                    }
                    Some(event) => {
                        i.policy.saturate();
                        (
                            callback,
                            vec![OwnerInputRejection { event, reason: RejectionReason::Saturated }],
                            false,
                        )
                    }
                }
            }
        };
        for e in rejected {
            callback(e);
        }
        accepted
    }
    /// Take explicit edit ack BEFORE flushing: consuming it frees the reserved
    /// composition slot. A frame/capacity bit itself is never treated as success.
    pub(crate) fn on_wake(input: &Rc<RefCell<Self>>) -> Result<(), TrySendError<()>> {
        let ack = {
            let mut i = input.borrow_mut();
            i.replies.take().map(|ack| (i.outcome.clone(), ack))
        };
        if let Some((callback, ack)) = ack {
            callback(ack);
        }
        let result = input.borrow_mut().flush();
        if matches!(result, Err(TrySendError::Disconnected(()))) {
            Self::shutdown(input);
        }
        result
    }
    pub(crate) fn shutdown(input: &Rc<RefCell<Self>>) {
        let (callback, events) = {
            let mut i = input.borrow_mut();
            i.policy.disconnect();
            (i.rejected.clone(), i.pending.drain(..).collect::<Vec<_>>())
        };
        for event in events {
            callback(OwnerInputRejection { event, reason: RejectionReason::HostDisconnected });
        }
    }
    pub(crate) fn readiness(&self) -> InputReadiness {
        self.policy.state()
    }
    pub(crate) fn take_pending(&mut self) -> Vec<OwnerEvent> {
        self.pending.drain(..).collect()
    }
    pub(crate) fn replay(&mut self, event: OwnerEvent) -> Result<(), OwnerEvent> {
        self.signal_quit(&event);
        if self.policy.state() == InputReadiness::Disconnected {
            return Err(event);
        }
        if matches!(self.flush(), Err(TrySendError::Disconnected(()))) {
            self.policy.disconnect();
            return Err(event);
        }
        let event = if self.pending.is_empty() {
            match self.sender.try_send(event) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(event)) => event,
                Err(TrySendError::Disconnected(event)) => {
                    self.policy.disconnect();
                    return Err(event);
                }
            }
        } else {
            event
        };
        if self.pending.len() == self.capacity {
            return Err(event);
        }
        self.pending.push_back(event);
        Ok(())
    }
    pub(crate) fn resume(&mut self) -> bool {
        self.policy.resume(self.pending.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        composition::{CompositionContext, CompositionState},
        editor_bridge::{EditorBridge, TextSource},
        editor_transport::{CompositionRequest, pending_editor_transport},
    };
    use rune_display::{FixedMetrics, InputEvent, Rect, TextSnapshot, host_updates};
    use rune_editor::{Document, RopeDocument, Session};
    fn bridge() -> EditorBridge<Session<RopeDocument>> {
        EditorBridge::new(
            Session::new(RopeDocument::empty(), 80, 24).unwrap(),
            FixedMetrics::default(),
            Rect { x: 0., y: 0., width: 640., height: 480. },
        )
        .unwrap()
        .with_text_source(TextSource::CommittedText)
        .unwrap()
    }
    fn request(b: &EditorBridge<Session<RopeDocument>>) -> CompositionRequest {
        let s = b.snapshot().unwrap();
        let v = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        let d = s.buffers.iter().find(|b| b.id == v.view.buffer).unwrap();
        let mut c = CompositionState::new().unwrap();
        c.set_context(Some(CompositionContext {
            window_id: v.id.as_u64(),
            buffer_id: d.id.as_u64(),
            publication_epoch: d.display_snapshot().revision(),
            owner_sequence: s.sequence,
            replacement: v.view.point..v.view.point,
        }))
        .unwrap();
        let token = c.begin().unwrap();
        CompositionRequest { token, committed: c.commit(token, "λ".into()).unwrap() }
    }
    #[test]
    fn explicit_ack_is_taken_before_retry_frees_composition_reservation() {
        let b = bridge();
        let first = request(&b);
        let token1 = first.token;
        let (p, _u) = host_updates();
        let (s, r, o) = pending_editor_transport(1, p);
        let acks = Rc::new(RefCell::new(Vec::new()));
        let got = acks.clone();
        let i = OwnerInput::new(
            s,
            r,
            Arc::new(AtomicBool::new(false)),
            Rc::new(|_| panic!("unexpected rejection")),
            Rc::new(move |ack| got.borrow_mut().push((ack.token, ack.outcome.is_ok()))),
        );
        let mut w = o.bind(b);
        assert!(OwnerInput::submit(&i, OwnerEvent::Composition(first)));
        w.try_step().unwrap().edit.unwrap();
        let second = request(w.bridge());
        let token2 = second.token;
        assert!(OwnerInput::submit(&i, OwnerEvent::Composition(second)));
        assert_eq!(i.borrow().pending.len(), 1);
        OwnerInput::on_wake(&i).unwrap();
        assert!(i.borrow().pending.is_empty());
        w.try_step().unwrap().edit.unwrap();
        OwnerInput::on_wake(&i).unwrap();
        assert_eq!(*acks.borrow(), vec![(token1, true), (token2, true)]);
        assert_eq!(
            {
                let s = w.bridge().snapshot().unwrap();
                s.views.iter().find(|v| v.id == s.selected_window).unwrap().view.point
            },
            2
        );
    }
    #[test]
    fn saturation_preserves_fifo_pauses_input_and_quit_remains_observable() {
        let b = bridge();
        let comp = request(&b);
        let token = comp.token;
        let (p, _u) = host_updates();
        let (s, r, _o) = pending_editor_transport(1, p);
        let rejected = Rc::new(RefCell::new(Vec::new()));
        let got = rejected.clone();
        let quit = Arc::new(AtomicBool::new(false));
        let i = OwnerInput::new(
            s,
            r,
            quit.clone(),
            Rc::new(move |e| got.borrow_mut().push(e)),
            Rc::new(|_| {}),
        );
        i.borrow_mut().capacity = 1;
        assert!(OwnerInput::submit(
            &i,
            OwnerEvent::Input(InputEvent::Resize { width: 800., height: 600., scale: 1. })
        ));
        assert!(OwnerInput::submit(&i, OwnerEvent::Composition(comp)));
        assert!(!OwnerInput::submit(
            &i,
            OwnerEvent::Input(InputEvent::TextCommit("late".into()))
        ));
        assert_eq!(i.borrow().readiness(), InputReadiness::Paused);
        assert!(!OwnerInput::submit(
            &i,
            OwnerEvent::Input(InputEvent::Key {
                key: "g".into(),
                text: None,
                control: true,
                meta: false,
                shift: false
            })
        ));
        assert!(quit.load(Ordering::Acquire));
        assert_eq!(rejected.borrow().len(), 2);
        OwnerInput::shutdown(&i);
        let rejected = rejected.borrow();
        assert!(matches!(&rejected[2].event,OwnerEvent::Composition(r) if r.token==token));
        assert_eq!(i.borrow().readiness(), InputReadiness::Disconnected);
    }
}
