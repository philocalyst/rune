//! Truthful Unicode-only buffer representation, not a writable shadow flag.
use crate::core::{env::{Env, sym}, gc::{Context, Rt}, object::Object};
use anyhow::Result;
use rune_macros::defun;
defvar!(ENABLE_MULTIBYTE_CHARACTERS, true);

#[defun]
fn set_buffer_multibyte<'a>(flag: Object<'a>, env: &Rt<Env>, cx: &'a Context) -> Result<Object<'a>> {
    // Authenticate the actual owner/buffer even for a no-op request.
    let multibyte = env.editor_gateway().is_multibyte(env.current_buffer_handle(), cx)?;
    if !flag.is_nil() && multibyte { return Ok(flag); }
    // No byte-buffer representation exists. Do not fake conversion by toggling
    // a Lisp variable or silently changing scalar/byte position interpretation.
    let data = crate::core::cons::Cons::new1(cx.add("Unibyte buffer storage is not implemented"), cx).into();
    Err(crate::eval::EvalError::signal(sym::RUNE_UNIMPLEMENTED.into(), data, cx).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_mode_and_constant_writes_match_gnu_supported_cases() {
        crate::assert_elprop!(r#"(list enable-multibyte-characters (default-value 'enable-multibyte-characters) (default-toplevel-value 'enable-multibyte-characters) (boundp 'enable-multibyte-characters) (special-variable-p 'enable-multibyte-characters) (set-buffer-multibyte t) (set-buffer-multibyte 7))"#);
        // Rune's lexical profile must be compared with explicit GNU lexical eval,
        // not the raw oracle's default dynamic eval.
        crate::assert_elprop!(r#"(eval '(list (funcall (function (lambda (enable-multibyte-characters) enable-multibyte-characters)) nil) enable-multibyte-characters) t)"#);
        for form in [
            "(setq enable-multibyte-characters nil)",
            "(setq enable-multibyte-characters t)",
            "(let ((enable-multibyte-characters nil)) enable-multibyte-characters)",
            "(set-default 'enable-multibyte-characters nil)",
            "(set-default-toplevel-value 'enable-multibyte-characters nil)",
            "(makunbound 'enable-multibyte-characters)",
        ] { crate::assert_elprop!("(progn (put 'setting-constant 'error-conditions '(setting-constant error)) (condition-case e {} (setting-constant e)))", form); }
    }
    #[test]
    fn contextual_mode_ignores_shadow_slots_and_failed_conversion_has_no_owner_edit() {
        use crate::core::{gc::RootSet, object::{NIL, TRUE}};
        use rune_core::macros::root;
        {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        env.vars.insert(sym::ENABLE_MULTIBYTE_CHARACTERS, NIL);
        assert_eq!(env.variable_value(sym::ENABLE_MULTIBYTE_CHARACTERS, cx).unwrap(), Some(TRUE));
        assert_eq!(env.toplevel_value(sym::ENABLE_MULTIBYTE_CHARACTERS, cx).unwrap(), Some(TRUE));
        }
        crate::editor_document::with_lisp_runtime_owner(80, 24, |mut host| {
            use rune_editor::{EditorHost, Document};
            host.evaluate_source(r#"(insert "λ🙂")"#).map_err(anyhow::Error::msg)?;
            let before = host.snapshot()?;
            assert!(host.evaluate_source("(set-buffer-multibyte nil)").is_err());
            let after = host.snapshot()?;
            assert_eq!(before.sequence, after.sequence);
            assert_eq!(before.buffers.iter().map(|b| b.document.to_text()).collect::<Vec<_>>(), after.buffers.iter().map(|b| b.document.to_text()).collect::<Vec<_>>());
            Ok(())
        }).unwrap();
    }
    #[test]
    fn unsupported_conversion_preserves_owner_text_and_mode() {
        crate::interpreter::assert_lisp(r#"(progn (erase-buffer) (insert "λ🙂") (goto-char 2) (list (condition-case e (set-buffer-multibyte nil) (rune-unimplemented (car e))) enable-multibyte-characters (buffer-string) (point) (let ((v (vector nil))) (aset v 0 v) (eq v (set-buffer-multibyte v)))))"#, r#"(rune-unimplemented t "λ🙂" 2 t)"#);
        crate::interpreter::assert_lisp(r#"(progn (defvaralias 'rune-mode-alias 'enable-multibyte-characters) (list rune-mode-alias (condition-case e (setq rune-mode-alias nil) (setting-constant e)) (condition-case e (let ((rune-mode-alias nil)) nil) (setting-constant e))))"#, "(t (setting-constant rune-mode-alias) (setting-constant rune-mode-alias))");
    }
}
