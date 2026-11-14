//! Native opt-in startup runs on the same persistent Env/Context owner.
use super::LispRuntimeHost;
impl LispRuntimeHost<'_, '_, '_> {
    /// Run exactly the product's ordinary bootstrap. No second runtime, cloned
    /// editor, extracted macros, or automatic claim of full GNU startup support.
    /// Call before binding the input worker: errors retain partial effects but
    /// abort initialization, and queued input must be rejected, never replayed.
    pub(crate) fn bootstrap_runtime(&mut self) -> Result<(), String> {
        crate::bootstrap(self.env, self.owner.cx)
            .map_err(|()| "Native Lisp bootstrap failed; see loader diagnostic".into())
    }
}
/// Production (not cfg(test)) diagnostic uses the same host factory and startup
/// method as GPUI, but deliberately never constructs an Application or Window.
pub(crate) fn check_native_bootstrap() -> anyhow::Result<()> {
    use rune_editor::{Command, Document, EditorHost};
    super::with_lisp_runtime_owner(80, 24, |mut host| {
        let selected = host.snapshot()?.selected_window;
        host.bootstrap_runtime().map_err(anyhow::Error::msg)?;
        let snapshot = host.snapshot()?;
        anyhow::ensure!(snapshot.selected_window == selected, "bootstrap replaced selected UI authority");
        let view = snapshot.views.iter().find(|v| v.id == selected).ok_or_else(|| anyhow::anyhow!("missing selected view"))?;
        let buffer_id = view.view.buffer;
        let before = snapshot.buffers.iter().find(|b| b.id == buffer_id).ok_or_else(|| anyhow::anyhow!("missing selected buffer"))?.document.to_text();
        drop(snapshot);
        host.evaluate_source("(if (keymapp (current-global-map)) nil (error \"missing active map\")) (setq native-bootstrap-check-state (list 42))").map_err(anyhow::Error::msg)?;
        host.execute(Command::Insert("λ🙂".into()))?;
        host.evaluate_source(&format!("(garbage-collect) (if (and (= (car native-bootstrap-check-state) 42) (= (buffer-size) {})) nil (error \"owner/Env diverged\"))", before.chars().count() + 2)).map_err(anyhow::Error::msg)?;
        host.execute(Command::Undo)?;
        let snapshot = host.snapshot()?;
        let after = snapshot.buffers.iter().find(|b| b.id == buffer_id).ok_or_else(|| anyhow::anyhow!("lost selected buffer"))?;
        anyhow::ensure!(after.document.to_text() == before, "native Undo did not restore shared buffer");
        Ok(())
    })
}
#[cfg(test)]
mod tests {
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn actual_bootstrap_load_failure_does_not_run_owner_callback_after_startup() {
        let mut ready = false;
        let result = with_lisp_runtime_owner(80, 24, |mut host| {
            host.env.set_var_context(crate::sym::LOAD_PATH, crate::core::object::NIL, host.owner.cx)?;
            host.bootstrap_runtime().map_err(anyhow::Error::msg)?;
            ready = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!ready);
    }
    #[cfg(feature = "native")]
    #[test]
    fn bootstrap_failure_preserves_unbound_fifo_and_late_event_ownership() {
        use rune_gpui::editor_transport::{OwnerEvent, pending_editor_transport};
        use std::sync::mpsc::TrySendError;
        let (publisher, _updates) = rune_display::host_updates();
        let (sender, mut replies, pending) = pending_editor_transport(4, publisher);
        sender.try_send(OwnerEvent::Evaluate { request_id: 701, source: "(insert \"never\")".into() }).unwrap();
        let mut unbound = Some(pending);
        let result = with_lisp_runtime_owner(80, 24, |mut host| {
            host.env.set_var_context(crate::sym::LOAD_PATH, crate::core::object::NIL, host.owner.cx)?;
            host.bootstrap_runtime().map_err(anyhow::Error::msg)?;
            let _ = unbound.take();
            Ok(())
        });
        assert!(result.is_err());
        let rejected = unbound.take().unwrap().reject_unbound();
        assert!(matches!(&rejected[..], [OwnerEvent::Evaluate {request_id:701,source}] if source == "(insert \"never\")"));
        assert!(matches!(sender.try_send(OwnerEvent::Evaluate { request_id:702,source:"late".into() }), Err(TrySendError::Disconnected(OwnerEvent::Evaluate{request_id:702,source})) if source=="late"));
        assert!(replies.take().is_none());
    }
    #[test]
    #[ignore = "real normal bootstrap qualification; run explicitly from repository directory"]
    fn real_bootstrap_keeps_same_owner_and_active_global_map() {
        with_lisp_runtime_owner(80, 24, |mut host| {
            let selected = host.snapshot()?.selected_window;
            host.bootstrap_runtime().map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.selected_window, selected);
            host.evaluate_source("(if (keymapp (current-global-map)) nil (error \"missing real active map\")) (setq native-bootstrap-persistent (list 42)) (insert \"native-after-bootstrap\")").map_err(anyhow::Error::msg)?;
            host.evaluate_source("(garbage-collect) (if (= (car native-bootstrap-persistent) 42) nil (error \"lost Env\"))").map_err(anyhow::Error::msg)?;
            let snapshot = host.snapshot()?;
            let view = snapshot.views.iter().find(|v| v.id == selected).unwrap();
            let buffer = snapshot.buffers.iter().find(|b| b.id == view.view.buffer).unwrap();
            assert!(buffer.document.to_text().ends_with("native-after-bootstrap"));
            Ok(())
        }).unwrap();
    }
}
