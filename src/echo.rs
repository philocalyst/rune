//! Owner-local echo requests. No Lisp pointers or claim of a painted echo area.
use crate::core::{gc::Context, object::{Object, NIL}};
use anyhow::{Result, ensure};
use rune_macros::defun;
pub(crate) const MAX_ECHO_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct EchoState {
    frame: Option<u64>,
    text: Option<String>,
    revision: u64,
}
#[derive(Debug)]
pub(crate) struct EchoSnapshot {
    pub(crate) frame: u64,
    pub(crate) text: Option<String>,
    pub(crate) revision: u64,
    pub(crate) displayed: bool,
}
impl EchoState {
    pub(crate) fn replace(&mut self, frame: u64, text: Option<String>) -> Result<()> {
        ensure!(text.as_ref().is_none_or(|value| value.len() <= MAX_ECHO_BYTES), "Echo request exceeds 16 MiB retention limit");
        let next = self.revision.checked_add(1).ok_or_else(|| anyhow::anyhow!("Echo revision exhausted"))?;
        self.frame = Some(frame);
        self.text = text.filter(|value| !value.is_empty());
        self.revision = next;
        Ok(())
    }
    pub(crate) fn snapshot(&self, frame: u64, displayed: bool) -> Result<EchoSnapshot> {
        let text = if self.frame == Some(frame) {
            self.text.as_ref().map(|value| {
                let mut copied = String::new();
                copied.try_reserve_exact(value.len())?;
                copied.push_str(value);
                Ok::<_, anyhow::Error>(copied)
            }).transpose()?
        } else { None };
        Ok(EchoSnapshot { frame, text, revision: self.revision, displayed })
    }
}

/// GNU batch terminals have no displayed echo-area message, even though native
/// requests/logging still occur. Do not pretend a GUI echo driver is attached.
#[defun]
fn current_message<'ob>(cx: &'ob Context) -> Result<Object<'ob>> {
    let snapshot = cx.echo_snapshot()?;
    Ok(if snapshot.displayed { snapshot.text.map_or(NIL, |text| cx.add(text)) } else { NIL })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn owner_request_is_real_shared_state_and_clear_survives_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::core::env::sym::init_symbols();
        crate::editfns::message(cx.add("%s"), &[cx.add("owned echo")], cx).unwrap();
        let before = cx.echo_snapshot().unwrap();
        assert_eq!(before.text.as_deref(), Some("owned echo"));
        assert!(!before.displayed);
        cx.garbage_collect(true);
        let moved = cx.echo_snapshot().unwrap();
        assert_eq!(moved.frame, before.frame);
        assert_eq!(moved.text.as_deref(), Some("owned echo"));
        let result = crate::editfns::message(NIL, &[], cx).unwrap();
        assert!(result.is_nil());
        let after = cx.echo_snapshot().unwrap();
        assert!(after.text.is_none());
        assert!(after.revision > before.revision);
        assert_eq!(before.text.as_deref(), Some("owned echo")); // Immutable snapshot, not authority.
        assert!(current_message(cx).unwrap().is_nil());
    }
    #[test]
    fn formatting_failure_and_revision_exhaustion_preserve_previous_state() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        crate::core::env::sym::init_symbols();
        crate::editfns::message(cx.add("ready"), &[], cx).unwrap();
        let before = cx.echo_snapshot().unwrap();
        assert!(crate::editfns::message(cx.add("%s"), &[], cx).is_err());
        let after = cx.echo_snapshot().unwrap();
        assert_eq!(after.text, before.text);
        assert_eq!(after.revision, before.revision);
        let mut state = EchoState { frame: Some(1), text: Some("kept".to_owned()), revision: u64::MAX };
        assert!(state.replace(1, None).is_err());
        assert_eq!(state.text.as_deref(), Some("kept"));
    }
    #[test]
    fn echo_bytes_do_not_root_or_leak_across_retired_owners() {
        let prior = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            crate::core::env::sym::init_symbols();
            crate::editfns::message(cx.add("prior owner"), &[], &cx).unwrap();
            cx.echo_snapshot().unwrap()
        };
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let fresh = cx.echo_snapshot().unwrap();
        assert!(fresh.text.is_none());
        assert_ne!(fresh.frame, prior.frame);
        assert_eq!(prior.text.as_deref(), Some("prior owner"));
        let mut state = EchoState::default();
        assert!(state.replace(1, Some("x".repeat(MAX_ECHO_BYTES + 1))).is_err());
        assert!(state.text.is_none());
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn message_nil_and_headless_observations_match_gnu() {
        for source in [
            r#"(list (message nil) (current-message))"#,
            r#"(list (message "hello") (current-message) (message nil) (current-message))"#,
            r#"(let ((noninteractive nil)) (list (message "two") (current-message) (message nil) (current-message)))"#,
            r#"(list (equal (message "") "") (equal (message "%s" "") "") (message nil 17))"#,
        ] { crate::assert_elprop!("{}", source); }
    }
}
