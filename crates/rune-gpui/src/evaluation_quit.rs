//! Request-scoped evaluator interruption. Contains no runtime or UI values.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
pub struct EvaluationQuit(Arc<Mutex<Option<Arc<AtomicBool>>>>);

/// Owner-only lifetime: dropping this scope closes admission for this token.
/// Each later evaluation gets a new atomic, so late requests cannot poison it.
pub struct EvaluationScope {
    channel: EvaluationQuit,
    token: Arc<AtomicBool>,
}
impl EvaluationQuit {
    /// Activate exactly one evaluation. No mutex guard is retained by the caller
    /// or across evaluator execution, hooks, GC, UI callbacks or publication.
    pub fn begin(&self) -> Result<EvaluationScope, &'static str> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err("an evaluator interruption scope is already active");
        }
        let token = Arc::new(AtomicBool::new(false));
        *slot = Some(token.clone());
        Ok(EvaluationScope { channel: self.clone(), token })
    }
    /// UI C-g affects only a currently admitted evaluation. An idle C-g is not
    /// carried into a future request; its ordinary editor event is independent.
    pub fn request(&self) -> bool {
        let slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(token) = slot.as_ref() else {
            return false;
        };
        token.store(true, Ordering::Release);
        true
    }
}
impl EvaluationScope {
    pub fn token(&self) -> Arc<AtomicBool> {
        self.token.clone()
    }
}
impl Drop for EvaluationScope {
    fn drop(&mut self) {
        let mut slot = self.channel.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &self.token)) {
            *slot = None;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_and_completed_requests_do_not_leak_to_next_evaluation() {
        let channel = EvaluationQuit::default();
        assert!(!channel.request());
        let first = channel.begin().unwrap();
        let old = first.token();
        assert!(!old.load(Ordering::Acquire));
        assert!(channel.request());
        assert!(old.swap(false, Ordering::AcqRel));
        assert!(!old.load(Ordering::Acquire));
        drop(first);
        assert!(!channel.request());
        let next = channel.begin().unwrap();
        let current = next.token();
        // Even a delayed holder writing the old atomic cannot affect the next.
        old.store(true, Ordering::Release);
        assert!(!current.load(Ordering::Acquire));
        assert!(channel.request());
        assert!(current.load(Ordering::Acquire));
    }
    #[test]
    fn overlapping_scope_rejected_and_drop_reopens_admission() {
        let channel = EvaluationQuit::default();
        let scope = channel.begin().unwrap();
        assert!(channel.begin().is_err());
        drop(scope);
        assert!(channel.begin().is_ok());
    }
}
