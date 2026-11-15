//! Dormant process-local correlation and revocation, not native authority.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub const MAX_RETAINED_ROW_BYTES: usize = 256;
pub const MAX_RETAINED_ROW_SCALARS: usize = 64;

// Never reset during this loaded module's process lifetime. No persistence/IPC claim.
static NEXT_INCARNATION: AtomicU64 = AtomicU64::new(1);

fn allocate_incarnation(counter: &AtomicU64) -> Result<u64, RetainedRowError> {
    counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        next.checked_add(1)
    }).map_err(|_| RetainedRowError::Exhausted)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetainedRowScope {
    pub sequence: u64,
    pub window: u64,
    pub buffer: u64,
    pub text_epoch: u64,
    pub source_start_scalar: u64,
    pub style_revision: u64,
    pub viewport_revision: u64,
    pub font_bank_generation: u64,
    pub shaping_policy_revision: u64,
}

/// Private fields prevent constructing a response key from arbitrary integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetainedRowKey {
    incarnation: u64,
    request: u64,
}

#[derive(Clone, Debug)]
pub struct RetainedRowRequest {
    key: RetainedRowKey,
    scope: RetainedRowScope,
    text: Arc<str>,
    source_end_scalar: u64,
    live: Arc<AtomicBool>,
}

impl RetainedRowRequest {
    #[cfg(test)]
    pub(crate) fn source_weak_for_test(&self) -> std::sync::Weak<str> {
        Arc::downgrade(&self.text)
    }

    /// Observational owner cancellation state, never native paint authority.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    pub fn key(&self) -> RetainedRowKey {
        self.key
    }
    pub fn scope(&self) -> RetainedRowScope {
        self.scope
    }
    /// Checked half-open scalar span; buffer-length authentication is external.
    pub fn source_range(&self) -> std::ops::Range<u64> {
        self.scope.source_start_scalar..self.source_end_scalar
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Revocable correlation lease. Liveness is NOT fresh owner/native validation.
#[derive(Clone, Debug)]
pub struct AcceptedRowToken {
    key: RetainedRowKey,
    ui_token: u64,
    live: Arc<AtomicBool>,
}

impl AcceptedRowToken {
    pub fn key(&self) -> RetainedRowKey {
        self.key
    }
    pub fn ui_token(&self) -> u64 {
        self.ui_token
    }
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedRowError {
    Busy,
    Closed,
    Exhausted,
    SourceBudget,
    SourceRangeOverflow,
    UnsupportedSource,
    UnknownRequest,
    Stale,
    InvalidToken,
}

/// Caller-driven invalidation result, not a fresh owner-authentication proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowStateObservation {
    pub pending_revoked: bool,
    pub accepted_live: bool,
}

#[derive(Debug)]
struct AcceptedRow {
    request: RetainedRowRequest,
    token: AcceptedRowToken,
}

/// One pending request and one accepted lease, never Default/Clone/reseedable.
#[derive(Debug)]
pub struct RetainedRowTransaction {
    incarnation: u64,
    last_id: u64,
    pending: Option<RetainedRowRequest>,
    accepted: Option<AcceptedRow>,
    closed: bool,
}

impl RetainedRowTransaction {
    pub fn new() -> Result<Self, RetainedRowError> {
        Ok(Self {
            incarnation: allocate_incarnation(&NEXT_INCARNATION)?,
            last_id: 0,
            pending: None,
            accepted: None,
            closed: false,
        })
    }

    pub fn begin(
        &mut self,
        scope: RetainedRowScope,
        text: &str,
    ) -> Result<RetainedRowRequest, RetainedRowError> {
        if self.closed {
            return Err(RetainedRowError::Closed);
        }
        if self.pending.is_some() {
            return Err(RetainedRowError::Busy);
        }
        if text.len() > MAX_RETAINED_ROW_BYTES {
            return Err(RetainedRowError::SourceBudget);
        }
        let scalar_count = text.chars().count();
        if scalar_count > MAX_RETAINED_ROW_SCALARS {
            return Err(RetainedRowError::SourceBudget);
        }
        let source_end_scalar = scope.source_start_scalar
            .checked_add(scalar_count as u64)
            .ok_or(RetainedRowError::SourceRangeOverflow)?;
        if text.chars().any(char::is_control) {
            return Err(RetainedRowError::UnsupportedSource);
        }
        let id = self.last_id.checked_add(1).ok_or(RetainedRowError::Exhausted)?;
        let request = RetainedRowRequest {
            key: RetainedRowKey { incarnation: self.incarnation, request: id },
            scope,
            text: Arc::from(text),
            source_end_scalar,
            live: Arc::new(AtomicBool::new(true)),
        };
        self.revoke_accepted();
        self.last_id = id;
        self.pending = Some(request.clone());
        Ok(request)
    }

    /// Freshly authenticated owner scope/source is still a caller obligation.
    pub fn complete(
        &mut self,
        key: RetainedRowKey,
        ui_token: u64,
        current_scope: RetainedRowScope,
        current_text: &str,
    ) -> Result<AcceptedRowToken, RetainedRowError> {
        if self.closed {
            return Err(RetainedRowError::Closed);
        }
        if self.pending.as_ref().map(|request| request.key) != Some(key) {
            return Err(RetainedRowError::UnknownRequest);
        }
        let request = self.pending.take().expect("matching pending request");
        if request.scope != current_scope || request.text.as_ref() != current_text {
            request.live.store(false, Ordering::Release);
            return Err(RetainedRowError::Stale);
        }
        if ui_token == 0 {
            request.live.store(false, Ordering::Release);
            return Err(RetainedRowError::InvalidToken);
        }
        let token = AcceptedRowToken {
            key,
            ui_token,
            live: request.live.clone(),
        };
        self.accepted = Some(AcceptedRow { request, token: token.clone() });
        Ok(token)
    }

    /// Observe freshly authenticated owner state even before a reply arrives.
    /// Mismatching pending work is revoked so a UI consumer can skip shaping it.
    pub fn observe_owner_state(&mut self, scope: RetainedRowScope, text: &str) -> RowStateObservation {
        let pending_revoked = self.pending.as_ref().is_some_and(|request| {
            request.scope != scope || request.text.as_ref() != text
        });
        if pending_revoked {
            if let Some(request) = self.pending.take() {
                request.live.store(false, Ordering::Release);
            }
        }
        RowStateObservation {
            pending_revoked,
            accepted_live: self.revalidate(scope, text),
        }
    }

    /// Explicit invalidation hook for edits/style/font/viewport changes.
    /// Returns true only if the current accepted source/scope remains identical.
    pub fn revalidate(&mut self, scope: RetainedRowScope, text: &str) -> bool {
        let valid = !self.closed && self.accepted.as_ref().is_some_and(|accepted| {
            accepted.request.scope == scope && accepted.request.text.as_ref() == text
        });
        if !valid {
            self.revoke_accepted();
        }
        valid
    }

    fn revoke_accepted(&mut self) {
        if let Some(accepted) = self.accepted.take() {
            accepted.token.live.store(false, Ordering::Release);
        }
    }

    pub fn cancel(&mut self) {
        if let Some(request) = self.pending.take() {
            request.live.store(false, Ordering::Release);
        }
        self.revoke_accepted();
    }

    pub fn close(&mut self) {
        self.cancel();
        self.closed = true;
    }
}

impl Drop for RetainedRowTransaction {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> RetainedRowScope {
        RetainedRowScope {
            sequence: 1,
            window: 2,
            buffer: 3,
            text_epoch: 4,
            source_start_scalar: 0,
            style_revision: 5,
            viewport_revision: 6,
            font_bank_generation: 7,
            shaping_policy_revision: 8,
        }
    }

    fn accepted(transaction: &mut RetainedRowTransaction) -> AcceptedRowToken {
        let request = transaction.begin(scope(), "same").unwrap();
        transaction.complete(request.key(), 123, scope(), "same").unwrap()
    }

    #[test]
    fn fresh_owner_observation_revokes_pending_before_completion() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let request = transaction.begin(scope(), "same").unwrap();
        let unchanged = transaction.observe_owner_state(scope(), "same");
        assert!(!unchanged.pending_revoked);
        assert!(request.is_live());
        let mut fresh = scope();
        fresh.sequence += 2; // Point-away/back can retain text and text epoch.
        let observed = transaction.observe_owner_state(fresh, "same");
        assert!(observed.pending_revoked);
        assert!(!observed.accepted_live);
        assert!(!request.is_live());
        assert!(matches!(transaction.complete(request.key(), 1, fresh, "same"),
            Err(RetainedRowError::UnknownRequest)));
        let replacement = transaction.begin(fresh, "same").unwrap();
        assert_ne!(replacement.key(), request.key());
    }

    #[test]
    fn source_and_accepted_observation_are_exact_and_closed_stays_closed() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let request = transaction.begin(scope(), "é").unwrap();
        assert!(transaction.observe_owner_state(scope(), "e\u{301}").pending_revoked);
        assert!(!request.is_live());
        let token = accepted(&mut transaction);
        assert!(transaction.observe_owner_state(scope(), "same").accepted_live);
        assert!(!transaction.observe_owner_state(scope(), "changed").accepted_live);
        assert!(!token.is_live());
        transaction.close();
        assert_eq!(transaction.observe_owner_state(scope(), "same"), RowStateObservation {
            pending_revoked: false, accepted_live: false,
        });
        assert!(matches!(transaction.begin(scope(), "same"), Err(RetainedRowError::Closed)));
    }

    #[test]
    fn pending_request_clones_observe_cancel_error_drop_and_accepted_revocation() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let canceled = transaction.begin(scope(), "x").unwrap();
        let clone = canceled.clone();
        assert!(clone.is_live());
        transaction.cancel();
        assert!(!canceled.is_live());
        assert!(!clone.is_live());
        let stale = transaction.begin(scope(), "x").unwrap();
        assert!(transaction.complete(stale.key(), 1, scope(), "y").is_err());
        assert!(!stale.is_live());
        let invalid = transaction.begin(scope(), "x").unwrap();
        assert!(transaction.complete(invalid.key(), 0, scope(), "x").is_err());
        assert!(!invalid.is_live());
        let accepted_request = transaction.begin(scope(), "x").unwrap();
        let token = transaction.complete(accepted_request.key(), 1, scope(), "x").unwrap();
        assert!(accepted_request.is_live());
        assert!(token.is_live());
        transaction.cancel();
        assert!(!accepted_request.is_live());
        assert!(!token.is_live());
        let dropped = transaction.begin(scope(), "x").unwrap();
        drop(transaction);
        assert!(!dropped.is_live());
    }

    #[test]
    fn recreation_cannot_accept_old_key_or_reused_ui_token() {
        let mut old = RetainedRowTransaction::new().unwrap();
        let old_request = old.begin(scope(), "same").unwrap();
        drop(old);
        let mut recreated = RetainedRowTransaction::new().unwrap();
        let fresh = recreated.begin(scope(), "same").unwrap();
        assert_ne!(old_request.key(), fresh.key());
        assert!(matches!(recreated.complete(old_request.key(), 123, scope(), "same"),
            Err(RetainedRowError::UnknownRequest)));
        let token = recreated.complete(fresh.key(), 123, scope(), "same").unwrap();
        assert_eq!(token.key(), fresh.key());
        assert_eq!(token.ui_token(), 123);
        assert!(token.is_live());
    }

    #[test]
    fn accepted_clones_revoke_on_cancel_close_drop_and_replacement() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let first = accepted(&mut transaction);
        let clone = first.clone();
        transaction.cancel();
        assert!(!first.is_live());
        assert!(!clone.is_live());
        let second = accepted(&mut transaction);
        transaction.begin(scope(), "new").unwrap();
        assert!(!second.is_live());
        transaction.cancel();
        let third = accepted(&mut transaction);
        transaction.close();
        assert!(!third.is_live());
        assert!(matches!(transaction.begin(scope(), "x"), Err(RetainedRowError::Closed)));
        let mut another = RetainedRowTransaction::new().unwrap();
        let fourth = accepted(&mut another);
        drop(another);
        assert!(!fourth.is_live());
    }

    #[test]
    fn every_scope_dimension_and_exact_source_revoke() {
        for field in 0..9 {
            let mut changed = scope();
            match field {
                0 => changed.sequence += 1,
                1 => changed.window += 1,
                2 => changed.buffer += 1,
                3 => changed.text_epoch += 1,
                4 => changed.source_start_scalar += 1,
                5 => changed.style_revision += 1,
                6 => changed.viewport_revision += 1,
                7 => changed.font_bank_generation += 1,
                _ => changed.shaping_policy_revision += 1,
            }
            let mut transaction = RetainedRowTransaction::new().unwrap();
            let token = accepted(&mut transaction);
            assert!(transaction.revalidate(scope(), "same"));
            assert!(!transaction.revalidate(changed, "same"));
            assert!(!token.is_live());
        }
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let token = accepted(&mut transaction);
        assert!(!transaction.revalidate(scope(), "different"));
        assert!(!token.is_live());
    }

    #[test]
    fn duplicate_late_and_matching_failed_results() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let old = transaction.begin(scope(), "x").unwrap();
        transaction.cancel();
        let fresh = transaction.begin(scope(), "x").unwrap();
        assert!(matches!(transaction.complete(old.key(), 1, scope(), "x"),
            Err(RetainedRowError::UnknownRequest)));
        assert!(matches!(transaction.complete(fresh.key(), 1, scope(), "y"),
            Err(RetainedRowError::Stale)));
        assert!(matches!(transaction.complete(fresh.key(), 1, scope(), "x"),
            Err(RetainedRowError::UnknownRequest)));
        let next = transaction.begin(scope(), "x").unwrap();
        assert!(matches!(transaction.complete(next.key(), 0, scope(), "x"),
            Err(RetainedRowError::InvalidToken)));
        assert!(matches!(transaction.complete(next.key(), 1, scope(), "x"),
            Err(RetainedRowError::UnknownRequest)));
    }

    #[test]
    fn bounded_admission_and_counter_exhaustion_are_nonmutating() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let token = accepted(&mut transaction);
        for text in ["x".repeat(65), "🙂".repeat(65)] {
            assert!(matches!(transaction.begin(scope(), &text), Err(RetainedRowError::SourceBudget)));
            assert!(token.is_live());
        }
        assert!(matches!(transaction.begin(scope(), "x\ny"), Err(RetainedRowError::UnsupportedSource)));
        transaction.last_id = u64::MAX;
        assert!(matches!(transaction.begin(scope(), "x"), Err(RetainedRowError::Exhausted)));
        assert!(token.is_live());
        let exhausted = AtomicU64::new(u64::MAX);
        assert_eq!(allocate_incarnation(&exhausted), Err(RetainedRowError::Exhausted));
        assert_eq!(exhausted.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn source_span_counts_scalars_not_bytes_or_graphemes() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let mut source_scope = scope();
        source_scope.source_start_scalar = 20;
        let request = transaction.begin(source_scope, "éa\u{301}🙂").unwrap();
        assert_eq!(request.source_range(), 20..24);
        assert_eq!(request.text().len(), 9);
    }

    #[test]
    fn source_overflow_preserves_lease_and_does_not_reserve_id() {
        let mut transaction = RetainedRowTransaction::new().unwrap();
        let token = accepted(&mut transaction);
        let previous_id = transaction.last_id;
        let mut end_scope = scope();
        end_scope.source_start_scalar = u64::MAX;
        assert!(matches!(transaction.begin(end_scope, "x"),
            Err(RetainedRowError::SourceRangeOverflow)));
        assert!(token.is_live());
        assert_eq!(transaction.last_id, previous_id);
        let empty = transaction.begin(end_scope, "").unwrap();
        assert_eq!(empty.source_range(), u64::MAX..u64::MAX);
        assert!(!token.is_live());
    }

    #[test]
    fn parallel_construction_uses_distinct_namespaces() {
        let handles: Vec<_> = (0..16).map(|_| std::thread::spawn(|| {
            let mut transaction = RetainedRowTransaction::new().unwrap();
            transaction.begin(scope(), "x").unwrap().key()
        })).collect();
        let mut keys = Vec::new();
        for handle in handles {
            let key = handle.join().unwrap();
            assert!(!keys.contains(&key));
            keys.push(key);
        }
    }
}
