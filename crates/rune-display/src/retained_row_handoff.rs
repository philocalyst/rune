//! Dormant bounded owner/UI handoff; no native shape or renderer activation.
pub use crate::row_activity::{RowActivity, RowActivityWait};
use crate::row_activity::Activity;
use crate::retained_row_transaction::{
    AcceptedRowToken, RetainedRowError, RetainedRowKey, RetainedRowRequest,
    RetainedRowScope, RetainedRowTransaction, RowStateObservation,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
};

const MAILBOX_CAPACITY: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowHandoffError {
    Transaction(RetainedRowError),
    Disconnected,
    Saturated,
    SnapshotMismatch,
    UnsupportedSnapshot,
}

#[derive(Clone, Copy, Debug)]
pub struct RetainedRowReply {
    pub key: RetainedRowKey,
    pub ui_token: u64,
}

/// A failed send returns the unconsumed reply. Nothing is silently retried.
#[derive(Debug)]
pub struct ReplySendError {
    pub error: RowHandoffError,
    pub reply: RetainedRowReply,
}

pub struct RowPreparationOwner {
    transaction: RetainedRowTransaction,
    ui_alive: Arc<AtomicBool>,
    owner_alive: Arc<AtomicBool>,
    owner_activity: Arc<Activity>,
    ui_activity: Arc<Activity>,
    requests: Option<SyncSender<RetainedRowRequest>>,
    replies: Option<Receiver<RetainedRowReply>>,
    acceptances: Option<SyncSender<AcceptedRowToken>>,
}

pub struct RowPreparationUi {
    alive: Arc<AtomicBool>,
    owner_alive: Arc<AtomicBool>,
    owner_activity: Arc<Activity>,
    ui_activity: Arc<Activity>,
    requests: Receiver<RetainedRowRequest>,
    replies: SyncSender<RetainedRowReply>,
    acceptances: Receiver<AcceptedRowToken>,
}

pub fn row_preparation_handoff()
    -> Result<(RowPreparationOwner, RowPreparationUi), RetainedRowError>
{
    let transaction = RetainedRowTransaction::new()?;
    let ui_alive = Arc::new(AtomicBool::new(true));
    let owner_alive = Arc::new(AtomicBool::new(true));
    let owner_activity = Arc::new(Activity::default());
    let ui_activity = Arc::new(Activity::default());
    let (request_sender, request_receiver) = mpsc::sync_channel(MAILBOX_CAPACITY);
    let (reply_sender, reply_receiver) = mpsc::sync_channel(MAILBOX_CAPACITY);
    let (acceptance_sender, acceptance_receiver) = mpsc::sync_channel(MAILBOX_CAPACITY);
    Ok((
        RowPreparationOwner {
            transaction,
            ui_alive: ui_alive.clone(),
            owner_alive: owner_alive.clone(),
            owner_activity: owner_activity.clone(),
            ui_activity: ui_activity.clone(),
            requests: Some(request_sender),
            replies: Some(reply_receiver),
            acceptances: Some(acceptance_sender),
        },
        RowPreparationUi {
            alive: ui_alive, owner_alive, owner_activity, ui_activity, requests: request_receiver, replies: reply_sender,
            acceptances: acceptance_receiver,
        },
    ))
}

impl RowPreparationOwner {
    pub fn wait_for_activity(&mut self) -> RowActivityWait<'_> {
        self.owner_activity.wait()
    }

    /// Captures one complete bounded snapshot before any request-state mutation.
    /// Sequence/window/buffer authentication is still the owner caller's duty.
    pub fn prepare_snapshot(
        &mut self,
        scope: RetainedRowScope,
        snapshot: &dyn crate::TextSnapshot,
    ) -> Result<RetainedRowKey, RowHandoffError> {
        if scope.source_start_scalar != 0 || scope.window == 0 || scope.buffer == 0 {
            return Err(RowHandoffError::UnsupportedSnapshot);
        }
        if snapshot.revision() != scope.text_epoch {
            return Err(RowHandoffError::SnapshotMismatch);
        }
        let length = snapshot.len_chars();
        if length > crate::retained_row_transaction::MAX_RETAINED_ROW_SCALARS {
            return Err(RowHandoffError::UnsupportedSnapshot);
        }
        let mut source = String::new();
        let mut characters = snapshot.chars_from(0);
        for _ in 0..length {
            let character = characters.next().ok_or(RowHandoffError::SnapshotMismatch)?;
            if source.len() + character.len_utf8()
                > crate::retained_row_transaction::MAX_RETAINED_ROW_BYTES
            {
                return Err(RowHandoffError::UnsupportedSnapshot);
            }
            source.push(character);
        }
        if characters.next().is_some() {
            return Err(RowHandoffError::SnapshotMismatch);
        }
        self.prepare(scope, &source)
    }

    /// Nonblocking. No Session borrow or caller callback is retained.
    pub fn prepare(&mut self, scope: RetainedRowScope, text: &str)
        -> Result<RetainedRowKey, RowHandoffError>
    {
        if self.requests.is_none() || !self.ui_alive.load(Ordering::Acquire) {
            self.close();
            return Err(RowHandoffError::Disconnected);
        }
        let sender = self.requests.as_ref().ok_or(RowHandoffError::Disconnected)?;
        let request = self.transaction.begin(scope, text)
            .map_err(RowHandoffError::Transaction)?;
        let key = request.key();
        match sender.try_send(request) {
            Ok(()) => { self.ui_activity.signal(); Ok(key) },
            Err(TrySendError::Full(_)) => {
                self.transaction.cancel();
                Err(RowHandoffError::Saturated)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.close();
                Err(RowHandoffError::Disconnected)
            }
        }
    }

    /// The caller supplies a fresh authenticated owner snapshot for THIS poll.
    /// A reply is removed once, even when correlation fails; no edit is replayed.
    pub fn poll(&mut self, scope: RetainedRowScope, text: &str)
        -> Result<Option<AcceptedRowToken>, RowHandoffError>
    {
        if self.requests.is_none() || !self.ui_alive.load(Ordering::Acquire) {
            self.close();
            return Err(RowHandoffError::Disconnected);
        }
        // Fresh owner state also invalidates accepted geometry when no reply arrives.
        self.transaction.revalidate(scope, text);
        let replies = self.replies.as_ref().ok_or(RowHandoffError::Disconnected)?;
        match replies.try_recv() {
            Ok(reply) => {
                self.ui_activity.signal(); // Reply capacity is available again.
                let accepted = self.transaction.complete(reply.key, reply.ui_token, scope, text)
                    .map_err(RowHandoffError::Transaction)?;
                let acceptances = self.acceptances.as_ref().ok_or(RowHandoffError::Disconnected)?;
                match acceptances.try_send(accepted.clone()) {
                    Ok(()) => { self.ui_activity.signal(); Ok(Some(accepted)) },
                    Err(TrySendError::Full(_)) => {
                        self.transaction.cancel();
                        Err(RowHandoffError::Saturated)
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        self.close();
                        Err(RowHandoffError::Disconnected)
                    }
                }
            },
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.close();
                Err(RowHandoffError::Disconnected)
            }
        }
    }

    /// Explicit owner-turn hook; never enqueues a replacement or replays an edit.
    pub fn observe_owner_state(&mut self, scope: RetainedRowScope, text: &str)
        -> Result<RowStateObservation, RowHandoffError>
    {
        if self.requests.is_none() || !self.ui_alive.load(Ordering::Acquire) {
            self.close();
            return Err(RowHandoffError::Disconnected);
        }
        let observed = self.transaction.observe_owner_state(scope, text);
        self.ui_activity.signal();
        Ok(observed)
    }

    pub fn invalidate(&mut self) {
        self.transaction.cancel();
        self.ui_activity.signal();
    }

    pub fn revalidate(&mut self, scope: RetainedRowScope, text: &str) -> bool {
        let valid = self.transaction.revalidate(scope, text);
        if !valid { self.ui_activity.signal(); }
        valid
    }

    pub fn close(&mut self) {
        self.owner_alive.store(false, Ordering::Release);
        self.transaction.close();
        self.requests = None;
        self.replies = None;
        self.acceptances = None;
        self.ui_activity.close();
        self.owner_activity.close();
    }
}

impl Drop for RowPreparationOwner {
    fn drop(&mut self) { self.close(); }
}

impl RowPreparationUi {
    pub fn wait_for_activity(&mut self) -> RowActivityWait<'_> {
        self.ui_activity.wait()
    }

    /// Reject every mailbox after either endpoint closes; release bounded backlog.
    fn connected(&self) -> bool {
        let connected = self.alive.load(Ordering::Acquire)
            && self.owner_alive.load(Ordering::Acquire);
        if !connected {
            let _ = self.requests.try_recv();
            let _ = self.acceptances.try_recv();
        }
        connected
    }

    pub fn close(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = self.requests.try_recv();
        let _ = self.acceptances.try_recv();
        self.ui_activity.close();
        self.owner_activity.close();
    }

    /// Consumes at most one queued request and suppresses already-canceled work.
    /// Cancellation can still race this observation; native preflight must recheck.
    pub fn next_live_request(&self) -> Result<Option<RetainedRowRequest>, RowHandoffError> {
        self.next_request().map(|request| request.filter(RetainedRowRequest::is_live))
    }

    /// Drains at most one receipt; revoked correlation never becomes presentation.
    pub fn next_live_acceptance(&self) -> Result<Option<AcceptedRowToken>, RowHandoffError> {
        self.next_acceptance().map(|token| token.filter(AcceptedRowToken::is_live))
    }

    /// Owner-issued correlation only; a reply alone is never acceptance.
    /// Queued tokens may already be revoked and must be checked by the consumer.
    pub fn next_acceptance(&self) -> Result<Option<AcceptedRowToken>, RowHandoffError> {
        if !self.connected() { return Err(RowHandoffError::Disconnected); }
        match self.acceptances.try_recv() {
            Ok(token) => { self.owner_activity.signal(); Ok(Some(token)) },
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RowHandoffError::Disconnected),
        }
    }

    pub fn next_request(&self) -> Result<Option<RetainedRowRequest>, RowHandoffError> {
        if !self.connected() { return Err(RowHandoffError::Disconnected); }
        match self.requests.try_recv() {
            Ok(request) => { self.owner_activity.signal(); Ok(Some(request)) },
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RowHandoffError::Disconnected),
        }
    }

    pub fn reply(&self, reply: RetainedRowReply) -> Result<(), ReplySendError> {
        if !self.connected() {
            return Err(ReplySendError { error: RowHandoffError::Disconnected, reply });
        }
        let result = self.replies.try_send(reply).map_err(|error| match error {
            TrySendError::Full(reply) => ReplySendError {
                error: RowHandoffError::Saturated, reply,
            },
            TrySendError::Disconnected(reply) => ReplySendError {
                error: RowHandoffError::Disconnected, reply,
            },
        });
        if result.is_ok() { self.owner_activity.signal(); }
        result
    }
}

impl Drop for RowPreparationUi {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> RetainedRowScope {
        RetainedRowScope {
            sequence: 1, window: 2, buffer: 3, text_epoch: 4,
            source_start_scalar: 0, style_revision: 5, viewport_revision: 6,
            font_bank_generation: 7, shaping_policy_revision: 8,
        }
    }

    #[derive(Default)]
    struct WakeCount(std::sync::atomic::AtomicUsize);
    impl std::task::Wake for WakeCount {
        fn wake(self: Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); }
    }

    fn poll_wake(future: RowActivityWait<'_>, waker: &std::task::Waker)
        -> std::task::Poll<RowActivity>
    {
        use std::future::Future;
        std::pin::pin!(future).as_mut().poll(&mut std::task::Context::from_waker(waker))
    }

    struct ParkedUiTask {
        _endpoint: Arc<std::sync::Mutex<RowPreparationUi>>,
    }
    impl std::task::Wake for ParkedUiTask { fn wake(self: Arc<Self>) {} }

    #[test]
    fn parked_task_reference_cycle_needs_actual_future_drop_or_external_close() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let endpoint_controller = Arc::new(std::sync::Mutex::new(ui));
        let task = Arc::new(ParkedUiTask { _endpoint: endpoint_controller.clone() });
        let weak_task = Arc::downgrade(&task);
        let waker = std::task::Waker::from(task);
        let mut endpoint = endpoint_controller.lock().unwrap();
        let mut wait = std::pin::pin!(endpoint.wait_for_activity());
        use std::future::Future;
        assert!(wait.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        drop(waker);
        // The still-pending registration holds the task which owns the endpoint.
        // Dropping external task references does NOT mean Future::drop executed.
        assert!(weak_task.upgrade().is_some());
        owner.close(); // Independently owned lifecycle controller breaks the graph.
        assert!(weak_task.upgrade().is_none());
        assert_eq!(wait.as_mut().poll(&mut std::task::Context::from_waker(
            &std::task::Waker::from(Arc::new(WakeCount::default())))),
            std::task::Poll::Ready(RowActivity::Disconnected));
    }

    #[test]
    fn explicit_owner_close_rejects_all_ui_mailboxes_and_releases_on_observation() {
        let (mut owner, mut ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "queued").unwrap();
        let request = ui.next_request().unwrap().unwrap();
        let source = request.source_weak_for_test();
        owner.requests.as_ref().unwrap().try_send(request).unwrap();
        owner.close();
        owner.close();
        assert!(source.upgrade().is_some()); // Receiver owns one bounded backlog.
        assert!(matches!(ui.next_acceptance(), Err(RowHandoffError::Disconnected)));
        assert!(source.upgrade().is_none()); // Receipt-only observation drains it.
        let failure = ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap_err();
        assert_eq!(failure.error, RowHandoffError::Disconnected);
        assert_eq!(failure.reply.key, key);
        assert!(matches!(ui.next_request(), Err(RowHandoffError::Disconnected)));
        ui.close();
        ui.close();
    }

    #[test]
    fn request_reply_receipt_capacity_and_close_wake_registered_consumers() {
        let (mut owner, mut ui) = row_preparation_handoff().unwrap();
        let count = Arc::new(WakeCount::default());
        let waker = std::task::Waker::from(count.clone());
        let key;
        {
            use std::future::Future;
            let mut wait = std::pin::pin!(ui.wait_for_activity());
            assert!(wait.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
            key = owner.prepare(scope(), "x").unwrap();
            assert_eq!(count.0.load(Ordering::SeqCst), 1);
            assert_eq!(wait.as_mut().poll(&mut std::task::Context::from_waker(&waker)),
                std::task::Poll::Ready(RowActivity::Ready));
        }
        assert!(poll_wake(owner.wait_for_activity(), &waker).is_pending());
        ui.next_live_request().unwrap().unwrap();
        assert_eq!(poll_wake(owner.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Ready));
        ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap();
        assert_eq!(poll_wake(owner.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Ready));
        owner.poll(scope(), "x").unwrap().unwrap();
        assert_eq!(poll_wake(ui.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Ready));
        ui.next_live_acceptance().unwrap().unwrap();
        assert_eq!(poll_wake(owner.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Ready));
        assert!(poll_wake(ui.wait_for_activity(), &waker).is_pending());
        owner.close();
        assert_eq!(poll_wake(ui.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Disconnected));
        assert_eq!(poll_wake(owner.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Disconnected));
    }

    #[test]
    fn ui_close_wakes_owner_but_revocation_still_requires_owner_observation() {
        let (mut owner, mut ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap();
        let token = owner.poll(scope(), "x").unwrap().unwrap();
        let waker = std::task::Waker::from(Arc::new(WakeCount::default()));
        // Consume coalesced capacity/reply activity before registering idle.
        assert!(poll_wake(owner.wait_for_activity(), &waker).is_ready());
        assert!(poll_wake(owner.wait_for_activity(), &waker).is_pending());
        ui.close();
        assert!(token.is_live());
        assert_eq!(poll_wake(owner.wait_for_activity(), &waker),
            std::task::Poll::Ready(RowActivity::Disconnected));
        assert!(matches!(owner.poll(scope(), "x"), Err(RowHandoffError::Disconnected)));
        assert!(!token.is_live());
    }

    #[test]
    fn owner_observation_suppresses_queued_stale_work_before_ui_shaping() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let old = owner.prepare(scope(), "x").unwrap();
        let mut fresh = scope();
        fresh.viewport_revision += 1;
        assert!(owner.observe_owner_state(fresh, "x").unwrap().pending_revoked);
        let worker = std::thread::spawn(move || {
            assert!(ui.next_live_request().unwrap().is_none());
            ui
        });
        let ui = worker.join().unwrap();
        let replacement = owner.prepare(fresh, "x").unwrap();
        assert_ne!(replacement, old);
        assert_eq!(ui.next_live_request().unwrap().unwrap().key(), replacement);
    }

    #[test]
    fn owner_observation_after_ui_drop_closes_instead_of_rescheduling() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        owner.prepare(scope(), "x").unwrap();
        drop(ui);
        assert_eq!(owner.observe_owner_state(scope(), "x"), Err(RowHandoffError::Disconnected));
        assert_eq!(owner.prepare(scope(), "x"), Err(RowHandoffError::Disconnected));
    }

    #[test]
    fn bounded_live_request_drain_skips_canceled_work_and_allows_explicit_retry() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        owner.prepare(scope(), "old").unwrap();
        owner.invalidate();
        let worker = std::thread::spawn(move || {
            assert!(ui.next_live_request().unwrap().is_none());
            ui
        });
        let ui = worker.join().unwrap();
        let fresh = owner.prepare(scope(), "new").unwrap();
        let request = ui.next_live_request().unwrap().unwrap();
        assert_eq!(request.key(), fresh);
        assert_eq!(request.text(), "new");
    }

    #[test]
    fn bounded_live_receipt_drain_discards_revoked_token_before_new_acceptance() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let first = owner.prepare(scope(), "x").unwrap();
        ui.next_live_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: first, ui_token: 1 }).unwrap();
        let old = owner.poll(scope(), "x").unwrap().unwrap();
        owner.invalidate();
        assert!(!old.is_live());
        assert!(ui.next_live_acceptance().unwrap().is_none());
        let fresh = owner.prepare(scope(), "y").unwrap();
        ui.next_live_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: fresh, ui_token: 2 }).unwrap();
        owner.poll(scope(), "y").unwrap().unwrap();
        let token = ui.next_live_acceptance().unwrap().unwrap();
        assert_eq!(token.key(), fresh);
        assert_eq!(token.ui_token(), 2);
        assert!(ui.next_live_acceptance().unwrap().is_none());
    }

    #[test]
    fn accepted_receipts_do_not_retain_source_after_owner_invalidation() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "resource-source").unwrap();
        let request = ui.next_request().unwrap().unwrap();
        let source = request.source_weak_for_test();
        ui.reply(RetainedRowReply { key, ui_token: 91 }).unwrap();
        drop(request);
        let owner_token = owner.poll(scope(), "resource-source").unwrap().unwrap();
        let ui_token = ui.next_acceptance().unwrap().unwrap();
        assert!(source.upgrade().is_some());
        owner.invalidate();
        assert!(!owner_token.is_live());
        assert!(!ui_token.is_live());
        assert!(source.upgrade().is_none(), "receipt flags must not own request source");
    }

    #[test]
    fn canceled_queued_source_is_released_when_ui_drains_last_request() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let old = owner.prepare(scope(), "queued-source").unwrap();
        // Obtain a Weak without retaining the payload: pop the real queue entry
        // temporarily, then put that SAME request back before cancellation.
        let request = ui.next_request().unwrap().unwrap();
        let source = request.source_weak_for_test();
        owner.requests.as_ref().unwrap().try_send(request).unwrap();
        owner.invalidate();
        assert!(source.upgrade().is_some(), "queued immutable data remains owned");
        let canceled = ui.next_request().unwrap().unwrap();
        assert_eq!(canceled.key(), old);
        assert!(!canceled.is_live());
        drop(canceled);
        assert!(source.upgrade().is_none());
        assert!(ui.next_request().unwrap().is_none());
    }

    #[test]
    fn owner_drop_revokes_but_does_not_artificially_own_external_request_source() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        owner.prepare(scope(), "external-source").unwrap();
        let request = ui.next_request().unwrap().unwrap();
        let source = request.source_weak_for_test();
        drop(owner);
        assert!(!request.is_live());
        assert!(source.upgrade().is_some(), "external clone still owns its data");
        assert!(matches!(ui.next_request(), Err(RowHandoffError::Disconnected)));
        drop(request);
        assert!(source.upgrade().is_none());
    }

    #[test]
    fn ui_receives_only_owner_accepted_full_key_and_revocation() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key, ui_token: 31 }).unwrap();
        assert!(ui.next_acceptance().unwrap().is_none());
        let owner_token = owner.poll(scope(), "x").unwrap().unwrap();
        let ui_token = ui.next_acceptance().unwrap().unwrap();
        assert_eq!(ui_token.key(), key);
        assert_eq!(ui_token.ui_token(), 31);
        assert!(ui_token.is_live());
        owner.invalidate();
        assert!(!ui_token.is_live());
        assert!(!owner_token.is_live());
        assert!(ui.next_acceptance().unwrap().is_none());
    }

    #[test]
    fn unread_acceptance_saturation_revokes_without_retry() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let first = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: first, ui_token: 1 }).unwrap();
        let old_token = owner.poll(scope(), "x").unwrap().unwrap();
        let second = owner.prepare(scope(), "y").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: second, ui_token: 2 }).unwrap();
        assert!(matches!(owner.poll(scope(), "y"), Err(RowHandoffError::Saturated)));
        let queued = ui.next_acceptance().unwrap().unwrap();
        assert_eq!(queued.key(), first);
        assert!(!queued.is_live());
        assert!(!old_token.is_live());
        assert!(ui.next_acceptance().unwrap().is_none());
        assert!(owner.poll(scope(), "y").unwrap().is_none());
        assert!(owner.prepare(scope(), "z").is_ok());
    }

    #[test]
    fn polling_fresh_state_revokes_even_without_a_matching_reply() {
        for late_reply in [false, true] {
            let (mut owner, ui) = row_preparation_handoff().unwrap();
            let old = owner.prepare(scope(), "x").unwrap();
            ui.next_request().unwrap().unwrap();
            owner.invalidate();
            let fresh = owner.prepare(scope(), "x").unwrap();
            ui.next_request().unwrap().unwrap();
            ui.reply(RetainedRowReply { key: fresh, ui_token: 1 }).unwrap();
            let token = owner.poll(scope(), "x").unwrap().unwrap();
            if late_reply {
                ui.reply(RetainedRowReply { key: old, ui_token: 2 }).unwrap();
            }
            let mut changed = scope();
            changed.style_revision += 1;
            let result = owner.poll(changed, "x");
            if late_reply {
                assert!(matches!(result, Err(RowHandoffError::Transaction(RetainedRowError::UnknownRequest))));
            } else {
                assert!(result.unwrap().is_none());
            }
            assert!(!token.is_live());
        }
    }

    #[test]
    fn immutable_snapshot_capture_checks_revision_and_exact_unicode_source() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let snapshot = crate::StringSnapshot::new("éa\u{301}🙂", scope().text_epoch);
        let key = owner.prepare_snapshot(scope(), &snapshot).unwrap();
        let request = ui.next_request().unwrap().unwrap();
        assert_eq!(request.key(), key);
        assert_eq!(request.text(), "éa\u{301}🙂");
        assert_eq!(request.source_range(), 0..4);
        owner.invalidate();
        let stale = crate::StringSnapshot::new("changed", scope().text_epoch + 1);
        assert_eq!(owner.prepare_snapshot(scope(), &stale), Err(RowHandoffError::SnapshotMismatch));
        assert!(ui.next_request().unwrap().is_none());
    }

    #[test]
    fn failed_snapshot_capture_does_not_revoke_accepted_lease() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap();
        let token = owner.poll(scope(), "x").unwrap().unwrap();
        let snapshot = crate::StringSnapshot::new("x".repeat(65), scope().text_epoch);
        assert_eq!(owner.prepare_snapshot(scope(), &snapshot), Err(RowHandoffError::UnsupportedSnapshot));
        assert!(token.is_live());
        let mut invalid = scope();
        invalid.source_start_scalar = 1;
        assert_eq!(owner.prepare_snapshot(invalid, &snapshot), Err(RowHandoffError::UnsupportedSnapshot));
        assert!(token.is_live());
    }

    #[test]
    fn inconsistent_snapshot_lengths_are_rejected_before_send() {
        struct Inconsistent { length: usize }
        impl crate::TextSnapshot for Inconsistent {
            fn revision(&self) -> u64 { scope().text_epoch }
            fn len_chars(&self) -> usize { self.length }
            fn chars_from(&self, _: usize) -> Box<dyn Iterator<Item = char> + '_> {
                Box::new("xy".chars())
            }
            fn next_line_start(&self, _: usize) -> Option<usize> { None }
        }
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        for length in [1, 3] {
            assert_eq!(owner.prepare_snapshot(scope(), &Inconsistent { length }),
                Err(RowHandoffError::SnapshotMismatch));
        }
        assert!(ui.next_request().unwrap().is_none());
    }

    #[test]
    fn actual_thread_handoff_correlates_once_and_revokes_on_close() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "a\u{301}b").unwrap();
        let worker = std::thread::spawn(move || {
            let request = ui.next_request().unwrap().unwrap();
            assert_eq!(request.key(), key);
            assert_eq!(request.text(), "a\u{301}b");
            ui.reply(RetainedRowReply { key, ui_token: 99 }).unwrap();
            ui
        });
        let ui = worker.join().unwrap();
        let accepted = owner.poll(scope(), "a\u{301}b").unwrap().unwrap();
        assert!(accepted.is_live());
        assert!(owner.poll(scope(), "a\u{301}b").unwrap().is_none());
        owner.close();
        assert!(!accepted.is_live());
        assert!(matches!(ui.next_request(), Err(RowHandoffError::Disconnected)));
    }

    #[test]
    fn delayed_old_reply_does_not_consume_fresh_pending_request() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let old = owner.prepare(scope(), "same").unwrap();
        ui.next_request().unwrap().unwrap();
        owner.invalidate();
        let fresh = owner.prepare(scope(), "same").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: old, ui_token: 1 }).unwrap();
        assert!(matches!(owner.poll(scope(), "same"),
            Err(RowHandoffError::Transaction(RetainedRowError::UnknownRequest))));
        ui.reply(RetainedRowReply { key: fresh, ui_token: 1 }).unwrap();
        assert!(owner.poll(scope(), "same").unwrap().unwrap().is_live());
    }

    #[test]
    fn bounded_queues_return_failures_without_hidden_retry() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let old = owner.prepare(scope(), "x").unwrap();
        owner.invalidate();
        assert!(matches!(owner.prepare(scope(), "y"), Err(RowHandoffError::Saturated)));
        let queued = ui.next_request().unwrap().unwrap();
        assert_eq!(queued.key(), old);
        let fresh = owner.prepare(scope(), "y").unwrap();
        ui.next_request().unwrap().unwrap();
        let reply = RetainedRowReply { key: fresh, ui_token: 1 };
        ui.reply(reply).unwrap();
        let failed = ui.reply(reply).unwrap_err();
        assert_eq!(failed.error, RowHandoffError::Saturated);
        assert_eq!(failed.reply.key, fresh);
        assert!(owner.poll(scope(), "y").unwrap().is_some());
        assert!(owner.poll(scope(), "y").unwrap().is_none());
    }

    #[test]
    fn stale_snapshot_and_endpoint_loss_fail_closed() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap();
        let mut changed = scope();
        changed.sequence += 1;
        assert!(matches!(owner.poll(changed, "x"),
            Err(RowHandoffError::Transaction(RetainedRowError::Stale))));
        let fresh = owner.prepare(changed, "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key: fresh, ui_token: 2 }).unwrap();
        let accepted = owner.poll(changed, "x").unwrap().unwrap();
        assert!(owner.revalidate(changed, "x"));
        drop(ui);
        assert!(matches!(owner.poll(changed, "x"), Err(RowHandoffError::Disconnected)));
        assert!(!accepted.is_live());
    }

    #[test]
    fn queued_reply_from_already_dropped_ui_is_not_accepted() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap();
        drop(ui);
        assert!(matches!(owner.poll(scope(), "x"), Err(RowHandoffError::Disconnected)));
    }

    #[test]
    fn owner_drop_revokes_and_returns_unsent_reply() {
        let (mut owner, ui) = row_preparation_handoff().unwrap();
        let key = owner.prepare(scope(), "x").unwrap();
        ui.next_request().unwrap().unwrap();
        ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap();
        let accepted = owner.poll(scope(), "x").unwrap().unwrap();
        drop(owner);
        assert!(!accepted.is_live());
        let failed = ui.reply(RetainedRowReply { key, ui_token: 1 }).unwrap_err();
        assert_eq!(failed.error, RowHandoffError::Disconnected);
        assert_eq!(failed.reply.key, key);
    }
}
