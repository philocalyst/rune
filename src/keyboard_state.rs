//! Native keyboard-macro state seed; no recorder/player is fabricated.
// GNU macros.c::init_macros initializes Vexecuting_kbd_macro to NIL. A future
// real player must bind the actual macro object and restore it on every exit.
// Physical owner key dispatch is not playback and must not set this variable.
defvar!(EXECUTING_KBD_MACRO);

#[cfg(test)]
mod tests {
    #[test]
    fn initialized_owner_keyboard_macro_state_is_nil_and_dynamic_values_are_real() {
        crate::interpreter::assert_lisp(r#"(list (boundp 'executing-kbd-macro) executing-kbd-macro (special-variable-p 'executing-kbd-macro) (local-variable-if-set-p 'executing-kbd-macro))"#, "(t nil t nil)");
        for form in [
            r#"(let ((v (vector 'macro-event))) (let ((executing-kbd-macro v)) (list (eq executing-kbd-macro v) (eq (default-value 'executing-kbd-macro) v))))"#,
            r#"(progn (setq executing-kbd-macro 7) (let ((executing-kbd-macro 'inner)) (list executing-kbd-macro (default-toplevel-value 'executing-kbd-macro))))"#,
            r#"(progn (setq executing-kbd-macro nil) (condition-case nil (let ((executing-kbd-macro [f6])) (signal 'error nil)) (error nil)) executing-kbd-macro)"#,
            r#"(progn (setq executing-kbd-macro 7) (makunbound 'executing-kbd-macro) (list (boundp 'executing-kbd-macro) (set-default 'executing-kbd-macro nil) executing-kbd-macro))"#,
        ] { crate::assert_elprop!("{}", form); }
    }
    #[test]
    fn owner_initialization_aliases_and_moving_collection_keep_actual_state() {
        crate::interpreter::assert_lisp(r#"(list (progn (defvaralias 'rune-macro-state-alias 'executing-kbd-macro) (let ((v (vector nil))) (aset v 0 v) (setq executing-kbd-macro v) (garbage-collect) (list (eq rune-macro-state-alias v) (let ((rune-macro-state-alias nil)) executing-kbd-macro) (eq executing-kbd-macro v)))) (condition-case e (defvaralias 'executing-kbd-macro 'rune-forbidden-macro-base) (error e)))"#, r#"((t nil t) (error "Cannot make a built-in variable an alias: executing-kbd-macro"))"#);
        crate::editor_document::with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source("(if executing-kbd-macro (signal 'error '(\"fresh owner is not playing a macro\"))) (setq rune-owner-macro-state (vector nil)) (aset rune-owner-macro-state 0 rune-owner-macro-state) (setq executing-kbd-macro rune-owner-macro-state) (garbage-collect)").map_err(anyhow::Error::msg)?;
            host.evaluate_source("(if (eq executing-kbd-macro rune-owner-macro-state) nil (signal 'error '(\"macro state identity lost\"))) (setq executing-kbd-macro nil)").map_err(anyhow::Error::msg)?;
            Ok(())
        }).unwrap();
    }
}
