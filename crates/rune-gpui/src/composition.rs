//! Captured-scope composition state. No platform InputHandler is registered by
//! this module. Recovery source, not recovered native runtime verification.
use std::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);
const MAX_TEXT_BYTES: usize = 65_536;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompositionToken {
    stream: u64,
    sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionContext {
    pub window_id: u64,
    pub buffer_id: u64,
    pub publication_epoch: u64,
    pub owner_sequence: u64,
    /// Exact Unicode scalar boundaries, converted by the owner-prepared UTF-16
    /// context. Never round a surrogate interior or substitute current point.
    pub replacement: Range<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedText {
    pub context: CompositionContext,
    pub text: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositionError {
    Exhausted,
    NoContext,
    Pending,
    StaleToken,
    InvalidRange,
    TooLarge,
}
struct Pending {
    token: CompositionToken,
    context: CompositionContext,
    preedit: String,
    emitted: bool,
}
pub struct CompositionState {
    stream: u64,
    sequence: u64,
    context: Option<CompositionContext>,
    pending: Option<Pending>,
}
impl CompositionState {
    pub fn new() -> Result<Self, CompositionError> {
        let mut stream = NEXT_STREAM.load(Ordering::Relaxed);
        loop {
            let next = stream.checked_add(1).ok_or(CompositionError::Exhausted)?;
            match NEXT_STREAM.compare_exchange_weak(
                stream,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => stream = actual,
            }
        }
        Ok(Self { stream, sequence: 0, context: None, pending: None })
    }
    pub fn set_context(
        &mut self,
        context: Option<CompositionContext>,
    ) -> Result<(), CompositionError> {
        if context.as_ref().is_some_and(|c| c.replacement.start > c.replacement.end) {
            return Err(CompositionError::InvalidRange);
        }
        if self.context != context {
            self.pending = None;
        }
        self.context = context;
        Ok(())
    }
    /// Call on every focus/document/caret/selection transition, including ABA.
    /// Cancellation after enqueue never means an editor edit was rolled back.
    pub fn invalidate(&mut self) {
        self.context = None;
        self.pending = None;
    }
    pub fn begin(&mut self) -> Result<CompositionToken, CompositionError> {
        if self.pending.is_some() {
            return Err(CompositionError::Pending);
        }
        let context = self.context.clone().ok_or(CompositionError::NoContext)?;
        self.sequence = self.sequence.checked_add(1).ok_or(CompositionError::Exhausted)?;
        let token = CompositionToken { stream: self.stream, sequence: self.sequence };
        self.pending = Some(Pending { token, context, preedit: String::new(), emitted: false });
        Ok(token)
    }
    pub fn preedit(
        &mut self,
        token: CompositionToken,
        text: String,
    ) -> Result<(), CompositionError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(CompositionError::TooLarge);
        }
        let p = self
            .pending
            .as_mut()
            .filter(|p| p.token == token && !p.emitted)
            .ok_or(CompositionError::StaleToken)?;
        p.preedit = text;
        Ok(())
    }
    /// A UI-local virtual edit may expand its original replacement while the
    /// same captured owner scope/epoch remains active. Never amend after emission.
    pub fn amend_replacement(
        &mut self,
        token: CompositionToken,
        replacement: Range<usize>,
    ) -> Result<(), CompositionError> {
        if replacement.start > replacement.end {
            return Err(CompositionError::InvalidRange);
        }
        let pending = self
            .pending
            .as_mut()
            .filter(|p| p.token == token && !p.emitted)
            .ok_or(CompositionError::StaleToken)?;
        pending.context.replacement = replacement;
        Ok(())
    }
    pub fn commit(
        &mut self,
        token: CompositionToken,
        text: String,
    ) -> Result<CommittedText, CompositionError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(CompositionError::TooLarge);
        }
        let p = self
            .pending
            .as_mut()
            .filter(|p| p.token == token && !p.emitted)
            .ok_or(CompositionError::StaleToken)?;
        p.emitted = true;
        Ok(CommittedText { context: p.context.clone(), text })
    }
    /// A rejected edit stays pending for explicit resolution; no optimistic clear.
    pub fn acknowledge(
        &mut self,
        token: CompositionToken,
        accepted: bool,
    ) -> Result<(), CompositionError> {
        if !self.pending.as_ref().is_some_and(|p| p.token == token && p.emitted) {
            return Err(CompositionError::StaleToken);
        }
        if accepted {
            self.pending = None;
        }
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> CompositionContext {
        CompositionContext {
            window_id: 1,
            buffer_id: 2,
            publication_epoch: 3,
            owner_sequence: 4,
            replacement: 1..2,
        }
    }
    #[test]
    fn emitted_commit_is_not_resubmitted_and_rejection_stays_pending() {
        let mut s = CompositionState::new().unwrap();
        s.set_context(Some(context())).unwrap();
        let t = s.begin().unwrap();
        s.preedit(t, "候補".into()).unwrap();
        assert_eq!(s.commit(t, "🙂".into()).unwrap().context, context());
        assert_eq!(s.commit(t, "duplicate".into()), Err(CompositionError::StaleToken));
        s.acknowledge(t, false).unwrap();
        assert_eq!(s.begin(), Err(CompositionError::Pending));
        s.acknowledge(t, true).unwrap();
        assert!(s.begin().is_ok());
    }
    #[test]
    fn invalidation_and_distinct_streams_reject_late_acknowledgments() {
        let mut a = CompositionState::new().unwrap();
        let mut b = CompositionState::new().unwrap();
        a.set_context(Some(context())).unwrap();
        b.set_context(Some(context())).unwrap();
        let t = a.begin().unwrap();
        assert_ne!(t, b.begin().unwrap());
        a.commit(t, "x".into()).unwrap();
        a.invalidate();
        a.set_context(Some(context())).unwrap();
        a.begin().unwrap();
        assert_eq!(a.acknowledge(t, true), Err(CompositionError::StaleToken));
    }
}
