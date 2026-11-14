//! Genuine owner overlay-arrow configuration; no arrow/fringe painting claim.
use crate::core::{env::INTERNED_SYMBOLS,gc::Context,object::{IntoObject,Object}};
defvar!(OVERLAY_ARROW_POSITION);
defvar!(OVERLAY_ARROW_STRING, crate::overlay_arrow::initial_arrow_string(cx));
defvar!(OVERLAY_ARROW_VARIABLE_LIST, list![sym::OVERLAY_ARROW_POSITION]);

pub(crate) fn initial_arrow_string<'a>(cx:&'a Context)->Object<'a>{
    // GNU xdisp.c uses build_pure_c_string. Allocate only this fixed immutable
    // leaf in the existing mutex-protected global block, never owner Lisp data.
    let symbols=INTERNED_SYMBOLS.lock().unwrap();
    let string="=>".into_obj(symbols.global_block());
    cx.bind(string).into()
}
#[cfg(test)]
mod tests{
    #[test]
    fn genuine_c_defaults_dynamic_aliases_and_mutable_registration_survive_gc(){
        crate::interpreter::assert_lisp(r#"(list overlay-arrow-position overlay-arrow-string overlay-arrow-variable-list (special-variable-p 'overlay-arrow-position) (special-variable-p 'overlay-arrow-string) (special-variable-p 'overlay-arrow-variable-list) (local-variable-if-set-p 'overlay-arrow-position))"#,r#"(nil "=>" (overlay-arrow-position) t t t nil)"#);
        crate::editor_document::with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source(r#"(setq rune-arrow-original overlay-arrow-variable-list) (defvaralias 'rune-arrow-list-alias 'overlay-arrow-variable-list) (let ((overlay-arrow-variable-list 42)) (if (= rune-arrow-list-alias 42) nil (signal 'error '("dynamic alias mismatch")))) (if (eq rune-arrow-original overlay-arrow-variable-list) nil (signal 'error '("list identity not restored"))) (setq overlay-arrow-variable-list (cons 'next-error-overlay-arrow-position overlay-arrow-variable-list)) (put 'next-error-overlay-arrow-position 'overlay-arrow-string "=>") (setcar rune-arrow-original 'rune-arrow-registered) (garbage-collect) (if (equal rune-arrow-list-alias '(next-error-overlay-arrow-position rune-arrow-registered)) nil (signal 'error '("mutable registration lost")))"#).map_err(anyhow::Error::msg)?;
            assert!(host.evaluate_source("(defvaralias 'overlay-arrow-position 'rune-arrow-invalid-base)").is_err());
            assert!(host.evaluate_source("(defvaralias 'overlay-arrow-string 'rune-arrow-invalid-base)").is_err());
            assert!(host.evaluate_source("(defvaralias 'overlay-arrow-variable-list 'rune-arrow-invalid-base)").is_err());
            Ok(())
        }).unwrap();
    }
    #[test]
    fn pure_default_string_rejects_mutation_while_replacement_and_raw_state_are_real(){
        crate::editor_document::with_lisp_runtime_owner(80,24,|mut host|{
            assert!(host.evaluate_source("(aset overlay-arrow-string 0 61)").is_err());
            assert!(host.evaluate_source("(clear-string overlay-arrow-string)").is_err());
            host.evaluate_source(r#"(if (equal overlay-arrow-string "=>") nil (signal 'error '("pure string changed"))) (setq rune-arrow-pure-string overlay-arrow-string) (setq overlay-arrow-string (copy-sequence overlay-arrow-string)) (clear-string overlay-arrow-string) (if (and (= (length overlay-arrow-string) 2) (= (aref overlay-arrow-string 0) 0) (= (aref overlay-arrow-string 1) 0) (equal rune-arrow-pure-string "=>")) nil (signal 'error '("replacement string is not independent and mutable"))) (setq rune-arrow-raw-state (vector nil)) (aset rune-arrow-raw-state 0 rune-arrow-raw-state) (setq overlay-arrow-position rune-arrow-raw-state) (garbage-collect) (if (eq overlay-arrow-position rune-arrow-raw-state) nil (signal 'error '("raw owner state identity lost")))"#).map_err(anyhow::Error::msg)?;
            Ok(())
        }).unwrap();
    }
}
