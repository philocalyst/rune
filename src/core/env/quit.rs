//! Owner-local token scopes. Only the plain request atomic crosses threads.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Default, Clone)]
pub(super) struct QuitControl(Rc<State>);
#[derive(Debug, Default)]
struct State {
    token: RefCell<Option<Arc<AtomicBool>>>,
    cleanup_depth: Cell<usize>,
}
/// Owns native state, not an Env/Context borrow; must be used in lexical scopes.
pub(crate) struct QuitTokenGuard {
    state: Rc<State>,
    previous: Option<Arc<AtomicBool>>,
}
impl Drop for QuitTokenGuard {
    fn drop(&mut self) {
        self.state.token.replace(self.previous.take());
    }
}
pub(crate) struct CleanupGuard(Rc<State>);
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.0.cleanup_depth.set(self.0.cleanup_depth.get() - 1);
    }
}
impl QuitControl {
    pub(super) fn install(&self, token: Arc<AtomicBool>) -> QuitTokenGuard {
        let previous = self.0.token.replace(Some(token));
        QuitTokenGuard { state: self.0.clone(), previous }
    }
    pub(super) fn cleanup(&self) -> CleanupGuard {
        self.0
            .cleanup_depth
            .set(self.0.cleanup_depth.get().checked_add(1).expect("cleanup depth"));
        CleanupGuard(self.0.clone())
    }
    pub(super) fn consume(&self, inhibited: bool) -> bool {
        if inhibited || self.0.cleanup_depth.get() != 0 {
            return false;
        }
        self.0
            .token
            .borrow()
            .as_ref()
            .is_some_and(|token| token.swap(false, Ordering::Relaxed))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_tokens_keep_pending_and_do_not_leak_to_next_request() {
        let control = QuitControl::default();
        let first = Arc::new(AtomicBool::new(true));
        {
            let _scope = control.install(first.clone());
            assert!(!control.consume(true));
            {
                let _cleanup = control.cleanup();
                assert!(!control.consume(false));
            }
            assert!(control.consume(false));
            assert!(!control.consume(false));
        }
        first.store(true, Ordering::Relaxed);
        let second = Arc::new(AtomicBool::new(false));
        let _scope = control.install(second);
        assert!(!control.consume(false));
    }
    #[test]
    fn nested_scope_restores_after_panic() {
        let control = QuitControl::default();
        let _outer = control.install(Arc::new(AtomicBool::new(true)));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = control.install(Arc::new(AtomicBool::new(false)));
            let _cleanup = control.cleanup();
            panic!("scope exits");
        }));
        assert!(control.consume(false));
    }
}
