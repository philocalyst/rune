//! Source-only proposed provide lifecycle regressions.

use super::*;
use crate::core::gc::RootSet;
use rune_core::macros::root;

fn assert_lisp(source: &str, expected: &str) {
    let roots = RootSet::default();
    let cx = &mut Context::new(&roots);
    sym::init_symbols();
    root!(env, init(Env::new_isolated(cx).unwrap()), cx);
    crate::core::env::init_variables(cx, env);
    let depth = env.binding_depth();
    let (form, consumed) = crate::reader::read(source, cx).unwrap();
    assert!(source[consumed..].trim().is_empty(), "trailing source forms");
    root!(form, cx);
    let value = crate::interpreter::eval(form, None, env, cx).unwrap();
    root!(value, cx);
    let (expected_value, consumed) = crate::reader::read(expected, cx).unwrap();
    assert!(expected[consumed..].trim().is_empty(), "trailing expected forms");
    assert_eq!(value.bind(cx), expected_value);
    assert_eq!(env.binding_depth(), depth);
}

#[test]
fn provide_after_load_immediate_order_gc() {
    assert_lisp("(let ((seen nil) (after-load-alist (list (list 'plan-feature (lambda () (garbage-collect) (setq seen (cons (featurep 'plan-feature) seen))) (lambda () (setq seen (cons 'second seen))))))) (list (provide 'plan-feature) seen))", "(plan-feature (second t))");
}

#[test]
fn provide_after_load_repeated_provide_dispatch() {
    assert_lisp("(let ((count 0) (after-load-alist (list (list 'plan-feature (lambda () (setq count (1+ count))))))) (provide 'plan-feature) (provide 'plan-feature) (list count (featurep 'plan-feature)))", "(2 t)");
}

#[test]
fn provide_after_load_first_symbol_entry_only() {
    assert_lisp("(let ((seen nil) (after-load-alist (list (list \"plan-feature\" (lambda () (setq seen 'string))) (list 'plan-feature (lambda () (setq seen 'first))) (list 'plan-feature (lambda () (setq seen 'duplicate)))))) (provide 'plan-feature) seen)", "first");
}

#[test]
fn provide_after_load_nested_distinct_feature_gc() {
    assert_lisp("(let ((seen nil) (after-load-alist (list (list 'plan-outer (lambda () (provide 'plan-inner) (garbage-collect) (setq seen (cons (featurep 'plan-outer) seen)))) (list 'plan-inner (lambda () (garbage-collect) (setq seen (cons 'inner seen))))))) (provide 'plan-outer) (list seen (featurep 'plan-inner)))", "((t inner) t)");
}

#[test]
fn provide_after_load_error_commits_feature_stops_later_callbacks() {
    assert_lisp("(let ((later nil) (after-load-alist (list (list 'plan-feature (lambda () (garbage-collect) (signal 'error nil)) (lambda () (setq later t)))))) (list (condition-case problem (provide 'plan-feature) (error (car problem))) (featurep 'plan-feature) later))", "(error t nil)");
}

#[test]
fn provide_after_load_throw_heap_identity_next_provide() {
    assert_lisp("(let ((payload (list 'heap)) (later nil) (after-load-alist (list (list 'plan-feature (lambda () (garbage-collect) (throw 'plan-exit payload)) (lambda () (setq later t)))))) (let ((answer (catch 'plan-exit (provide 'plan-feature)))) (garbage-collect) (setq after-load-alist nil) (list (eq answer payload) (featurep 'plan-feature) later (provide 'plan-next))))", "(t t nil plan-next)");
}

#[test]
fn provide_after_load_append_does_not_extend_initial_length() {
    assert_lisp("(let ((seen nil) (callbacks nil) (after-load-alist nil)) (fset 'plan-append '(lambda () (setq seen (cons 'first seen)) (setcdr callbacks (list 'plan-later)))) (fset 'plan-later '(lambda () (setq seen (cons 'appended seen)))) (setq callbacks (list 'plan-append)) (setq after-load-alist (list (cons 'plan-feature callbacks))) (provide 'plan-feature) seen)", "(first)");
}

#[test]
fn provide_after_load_shortening_stops_early() {
    assert_lisp("(let ((seen nil) (callbacks nil) (after-load-alist nil)) (fset 'plan-shorten '(lambda () (setq seen (cons 'first seen)) (setcdr callbacks nil))) (fset 'plan-later '(lambda () (setq seen (cons 'second seen)))) (setq callbacks (list 'plan-shorten 'plan-later)) (setq after-load-alist (list (cons 'plan-feature callbacks))) (provide 'plan-feature) seen)", "(first)");
}

#[test]
fn provide_after_load_dotted_prevalidation_no_callback() {
    assert_lisp("(let ((seen nil) (after-load-alist (list (cons 'plan-feature (cons (lambda () (setq seen t)) 17))))) (list (condition-case problem (provide 'plan-feature) (error (car problem))) seen (featurep 'plan-feature)))", "(caught nil t)");
}

#[test]
fn provide_after_load_cyclic_prevalidation_no_callback() {
    assert_lisp("(let ((seen nil) (callbacks nil) (after-load-alist nil)) (setq callbacks (list (lambda () (setq seen t)))) (setcdr callbacks callbacks) (setq after-load-alist (list (cons 'plan-feature callbacks))) (list (condition-case problem (provide 'plan-feature) (error (car problem))) seen (featurep 'plan-feature)))", "(caught nil t)");
}

#[test]
fn provide_after_load_history_before_callback() {
    assert_lisp("(let ((current-load-list (list \"origin.el\")) (seen nil) (after-load-alist (list (list 'pa-history (lambda () (setq seen (equal (car current-load-list) '(provide . pa-history)))))))) (provide 'pa-history) (list seen current-load-list))", "(t ((provide . pa-history) \"origin.el\"))");
}

#[test]
fn provide_after_load_fixed_length_ignores_extension() {
    assert_lisp("(let ((seen nil) (callbacks nil) (after-load-alist nil)) (setq callbacks (list (lambda () (setq seen (cons 'first seen)) (setcdr callbacks (list (lambda () (setq seen (cons 'appended seen)))))))) (setq after-load-alist (list (cons 'pa-extend callbacks))) (provide 'pa-extend) seen)", "(first)");
}

#[test]
fn provide_after_load_live_tail_replacement_and_truncation() {
    assert_lisp("(let ((seen nil) (callbacks nil) (after-load-alist nil)) (setq callbacks (list (lambda () (setq seen (cons 'first seen)) (setcdr callbacks (list (lambda () (setq seen (cons 'replacement seen)))))) (lambda () (signal 'error nil)))) (setq after-load-alist (list (cons 'pa-replace callbacks))) (provide 'pa-replace) seen)", "(replacement first)");
}

#[test]
fn provide_after_load_malformed_callbacks_commit_feature_without_invocation() {
    assert_lisp("(let ((current-load-list nil) (seen nil) (after-load-alist (list (cons 'pa-bad (cons (lambda () (setq seen t)) 7))))) (list (condition-case nil (provide 'pa-bad) (error 'caught)) seen (featurep 'pa-bad) current-load-list))", "(caught nil t ((provide . pa-bad)))");
}

#[test]
fn provide_after_load_vector_callbacks_keep_order() {
    assert_lisp("(let ((seen nil) (after-load-alist (list (cons 'pa-vector (vector (lambda () (setq seen (cons 'first seen))) (lambda () (garbage-collect) (setq seen (cons 'second seen)))))))) (provide 'pa-vector) seen)", "(second first)");
}

#[test]
fn provide_after_load_uninterned_feature_subfeatures_and_history_survive_gc() {
    assert_lisp("(let* ((current-load-list nil) (feature (make-symbol \"pa-unique\")) (sub (list 'part)) (seen nil) (after-load-alist (list (list feature (lambda () (garbage-collect) (setq seen (and (featurep feature) (eq (get feature 'subfeatures) sub) (eq (cdr (car current-load-list)) feature)))))))) (let ((result (provide feature sub))) (garbage-collect) (list seen (eq result feature) (eq (get feature 'subfeatures) sub))))", "(t t t)");
}

#[test]
fn provide_after_load_callback_created_cycle_stops_at_initial_length() {
    assert_lisp("(let ((count 0) (callbacks nil) (after-load-alist nil)) (setq callbacks (list (lambda () (setq count (1+ count)) (setcdr callbacks callbacks) (garbage-collect)) (lambda () (signal 'error nil)))) (setq after-load-alist (list (cons 'pa-created-cycle callbacks))) (provide 'pa-created-cycle) (setcdr callbacks nil) (list count (featurep 'pa-created-cycle)))", "(2 t)");
}

#[test]
fn provide_after_load_callback_created_dotted_tail_stops_without_error() {
    assert_lisp("(let ((count 0) (callbacks nil) (after-load-alist nil)) (setq callbacks (list (lambda () (setq count (1+ count)) (setcdr callbacks 7) (garbage-collect)) (lambda () (signal 'error nil)))) (setq after-load-alist (list (cons 'pa-created-dot callbacks))) (list (provide 'pa-created-dot) count (cdr callbacks) (featurep 'pa-created-dot)))", "(pa-created-dot 1 7 t)");
}

#[test]
fn provide_subfeatures_are_visible_before_callbacks_and_nil_preserves_identity() {
    assert_lisp("(let* ((sub (list 'part)) (seen nil) (after-load-alist (list (list 'pa-sub (lambda () (garbage-collect) (setq seen (and (featurep 'pa-sub 'part) (eq sub (get 'pa-sub 'subfeatures))))))))) (provide 'pa-sub sub) (provide 'pa-sub nil) (list seen (eq sub (get 'pa-sub 'subfeatures)) (condition-case nil (provide 'pa-invalid 7) (error 'caught)) (featurep 'pa-invalid)))", "(t t caught nil)");
}
