//! Newly proposed ownership regressions. No execution claim.
use super::*;
use crate::core::gc::RootSet;
use rune_core::macros::root;
#[test]
fn closures_preserve_shared_cache_and_identity_keys_across_gc() {
    crate::interpreter::assert_lisp(r#"(eval '(let* ((cache (make-hash-table :test 'eql)) (key (cons 'tag nil)) (f (function (lambda () (puthash key key cache)))))
      (fset 'owner-cache-a f) (fset 'owner-cache-b f)
      (owner-cache-a) (garbage-collect) (garbage-collect)
      (list (eq f (symbol-function 'owner-cache-a)) (eq f (symbol-function 'owner-cache-b))
            (eq key (owner-cache-b)) (eq key (gethash key cache))
            (gethash (cons 'tag nil) cache) (hash-table-count cache))) t)"#, "(t t t t nil 1)");
}
#[test]
fn aliases_introspection_and_redefinition_use_same_authority() {
    crate::interpreter::assert_lisp(r#"(progn
      (fset 'owner-target '(lambda (x) x)) (fset 'owner-alias 'owner-target)
      (let ((first (list (owner-alias 4) (funcall 'owner-alias 5) (apply 'owner-alias '(6))
                        (functionp 'owner-alias) (func-arity 'owner-alias)
                        (eq (indirect-function 'owner-alias) (symbol-function 'owner-target)))))
        (fset 'owner-target '(lambda (x) (+ x 1))) (list first (owner-alias 9))))"#, "((4 5 6 t (1 . 1) t) 10)");
}
#[test]
fn alias_cycle_rejection_keeps_previous_definition() {
    crate::interpreter::assert_lisp(r#"(progn (fset 'owner-cyclic-a '(lambda () 8)) (fset 'owner-cyclic-b 'owner-cyclic-a)
      (list (condition-case nil (fset 'owner-cyclic-a 'owner-cyclic-b) (cyclic-function-indirection 'caught))
            (owner-cyclic-a) (owner-cyclic-b)))"#, "(caught 8 8)");
}
#[test]
fn macro_and_interactive_introspection_resolve_owner_cells() {
    crate::interpreter::assert_lisp(r#"(progn (fset 'owner-macro '(macro lambda (x) (list 'quote x)))
      (fset 'owner-command '(lambda () (interactive) 19))
      (list (macroexpand '(owner-macro token)) (owner-macro token)
            (commandp 'owner-command) (call-interactively 'owner-command)))"#, "('token token t 19)");
}
#[test]
fn builtin_tombstone_and_redefinition_do_not_modify_other_env() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(a, init(Env::new(cx).unwrap()), cx); root!(b, init(Env::new(cx).unwrap()), cx);
    let builtin = sym::IDENTITY;
    root!(original, symbol_function_owner(builtin, b, cx), cx);
    assert!(!original.bind(cx).is_nil());
    fset_owner(builtin, NIL, a, cx).unwrap();
    assert!(symbol_function_owner(builtin, a, cx).is_nil());
    assert!(symbol_function_owner(builtin, b, cx).ptr_eq(original.bind(cx)));
    fset_owner(builtin, Object::from(sym::CAR), a, cx).unwrap();
    assert!(resolve_owner_function(builtin.into(), a, cx).unwrap().ptr_eq(symbol_function_owner(sym::CAR, b, cx)));
    fmakunbound_owner(builtin, a, cx).unwrap();
    cx.garbage_collect(true);
    assert!(symbol_function_owner(builtin, a, cx).is_nil());
    assert!(symbol_function_owner(builtin, b, cx).ptr_eq(original.bind(cx)));
}
#[test]
fn uninterned_function_aliases_are_traced_and_owner_dump_rejects() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    root!(a, Symbol::new_uninterned("owner-a", &cx.block), cx);
    root!(b, Symbol::new_uninterned("owner-b", &cx.block), cx);
    fset_owner(b.bind(cx), Object::from(sym::IDENTITY), env, cx).unwrap();
    fset_owner(a.bind(cx), Object::from(b.bind(cx)), env, cx).unwrap();
    for _ in 0..3 { cx.garbage_collect(true); assert!(owner_callable(a.bind(cx), env, cx).unwrap().is_some()); }
    let path = std::env::temp_dir().join(format!("owner-function-dump-{}", std::process::id()));
    assert!(!path.exists());
    assert!(crate::dump::dump_to_file(&path, env, cx).is_err());
    assert!(!path.exists());
}
#[test]
fn resolver_and_publication_agree_on_256_edges_and_atomic_excess() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    let symbols: Vec<_> = (0..257).map(|i| crate::core::env::intern(&format!("owner-boundary-{i}"), cx)).collect();
    let native = symbol_function_owner(sym::IDENTITY, env, cx);
    fset_owner(symbols[255], native, env, cx).unwrap();
    for i in (0..255).rev() { fset_owner(symbols[i], symbols[i+1].into(), env, cx).unwrap(); }
    assert!(resolve_owner_function(symbols[0].into(), env, cx).unwrap().ptr_eq(native));
    assert!(owner_callable(symbols[0], env, cx).unwrap().is_some());
    fset_owner(symbols[256], native, env, cx).unwrap();
    assert!(fset_owner(symbols[256], symbols[0].into(), env, cx).is_err());
    assert!(symbol_function_owner(symbols[256], env, cx).ptr_eq(native));
}
#[test]
fn symbol_funcall_and_apply_autoload_actual_source() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    crate::core::env::init_variables(cx, env);
    let dir = std::env::temp_dir().join(format!("owner-autoload-dispatch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, call) in [("owner-auto-funcall", "(funcall 'owner-auto-funcall 41)"), ("owner-auto-apply", "(apply 'owner-auto-apply '(41))")] {
        let file = dir.join(format!("{name}.el"));
        std::fs::write(&file, format!("(fset '{name} '(lambda (x) (+ x 1)))")).unwrap();
        let metadata = list![sym::AUTOLOAD, file.to_str().unwrap(), "metadata", NIL, NIL; cx];
        fset_owner(crate::core::env::intern(name, cx), metadata, env, cx).unwrap();
        let form = crate::reader::read(call, cx).unwrap().0; root!(form, cx);
        assert_eq!(crate::interpreter::eval(form, None, env, cx).unwrap(), Object::from(42));
    }
    // A file that never defines the target must error rather than recurse.
    let file = dir.join("unresolved.el"); std::fs::write(&file, "nil").unwrap();
    let metadata = list![sym::AUTOLOAD, file.to_str().unwrap(); cx];
    fset_owner(crate::core::env::intern("owner-unresolved-auto", cx), metadata, env, cx).unwrap();
    let form = crate::reader::read("(funcall 'owner-unresolved-auto)", cx).unwrap().0; root!(form, cx);
    assert!(crate::interpreter::eval(form, None, env, cx).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn uninterned_function_cycle_releases_native_capture_exactly_once() {
    use crate::{channel_manager::{ChannelManager, RecvError}, core::object::make_channel_pair_for_test};
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    let manager = ChannelManager::isolated_for_test();
    let (id, sender, receiver) = make_channel_pair_for_test(manager.clone(), 1);
    {
        root!(name, Symbol::new_uninterned("owner-finalizable-cycle", &cx.block), cx);
        let sender = cx.add(sender);
        let function = list![sym::LAMBDA, NIL, list![sym::QUOTE, sender; cx], list![sym::QUOTE, name.bind(cx); cx]; cx];
        fset_owner(name.bind(cx), function, env, cx).unwrap();
        cx.garbage_collect(true);
        assert_eq!(owner_function_table(env, cx).unwrap().len(), 1);
        assert_eq!(manager.counts_for_test(id), Some((1, 1)));
    }
    for _ in 0..3 {
        cx.garbage_collect(true);
        assert_eq!(owner_function_table(env, cx).unwrap().len(), 0);
        assert_eq!(manager.counts_for_test(id), Some((0, 1)));
    }
    assert!(matches!(receiver.try_recv(cx), Err(RecvError::Closed)));
    receiver.close();
}
#[test]
fn rooted_alias_activates_uninterned_target_in_weak_fixed_point() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    {
        root!(first, Symbol::new_uninterned("owner-live-alias", &cx.block), cx);
        let target = Symbol::new_uninterned("owner-conditional-target", &cx.block);
        // Insert target first: its conditional activation needs another round.
        fset_owner(target, list![sym::LAMBDA, NIL, Object::from(7); cx], env, cx).unwrap();
        fset_owner(first.bind(cx), target.into(), env, cx).unwrap();
        for _ in 0..3 {
            cx.garbage_collect(true);
            assert!(owner_callable(first.bind(cx), env, cx).unwrap().is_some());
            assert_eq!(owner_function_table(env, cx).unwrap().len(), 2);
        }
    }
    cx.garbage_collect(true);
    assert_eq!(owner_function_table(env, cx).unwrap().len(), 0);
}
#[test]
fn legacy_function_dump_rejects_before_namespace_mutation() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    root!(name, crate::core::env::intern("owner-legacy-import", cx), cx);
    root!(before, symbol_function_owner(sym::IDENTITY, env, cx), cx);
    fset_owner(name.bind(cx), before.bind(cx), env, cx).unwrap();
    let text = ".HEADER\n  version 1\n  objects 3\n\n.OBJECTS\n  @0 nil\n  @1 symbol \"owner-legacy-import\" interned=true\n  @2 cons car=@0 cdr=@0\n\n.SYMBOLS\n  \"owner-legacy-import\" -> @1 func=@2\n\n.ENV\n";
    let path = std::env::temp_dir().join(format!("owner-legacy-dump-{}", std::process::id()));
    std::fs::write(&path, text).unwrap();
    let error = crate::dump::load_dump(&path, env, cx).unwrap_err();
    assert!(error.contains("owner-aware schema"));
    assert!(symbol_function_owner(name.bind(cx), env, cx).ptr_eq(before.bind(cx)));
    assert!(symbol_function(name.bind(cx), cx).is_nil());
    std::fs::remove_file(path).unwrap();
}
#[test]
fn noncanonical_native_dump_mapping_is_not_silently_discarded() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    root!(identity, symbol_function_owner(sym::IDENTITY, env, cx), cx);
    let path = std::env::temp_dir().join(format!("owner-native-dump-{}", std::process::id()));
    for name in ["identity", "owner-new-native-alias"] {
        let text = format!(".HEADER\n  version 1\n  objects 2\n\n.OBJECTS\n  @0 symbol \"{name}\" interned=true\n  @1 subr \"car\"\n\n.SYMBOLS\n  \"{name}\" -> @0 func=@1\n\n.ENV\n");
        std::fs::write(&path, text).unwrap();
        assert!(crate::dump::load_dump(&path, env, cx).unwrap_err().contains("Noncanonical native"));
        assert!(symbol_function_owner(sym::IDENTITY, env, cx).ptr_eq(identity.bind(cx)));
    }
    std::fs::remove_file(path).unwrap();
}
#[test]
fn feature_only_namespace_dump_rejects_before_creating_output() {
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    env.provide_feature(crate::core::env::intern("owner-feature-only", cx), cx);
    assert!(!owner_functions_present(env, cx));
    let path = std::env::temp_dir().join(format!("owner-feature-only-dump-{}", std::process::id()));
    assert!(!path.exists());
    let error = crate::dump::dump_to_file(&path, env, cx).unwrap_err();
    assert!(error.to_string().contains("Owner feature"));
    assert!(!path.exists());
}
#[test]
fn direct_funcall_and_apply_follow_valid_two_stage_autoloads() {
    crate::sym::init_symbols();
    let roots = RootSet::default(); let cx = &mut Context::new(&roots);
    root!(env, init(Env::new(cx).unwrap()), cx);
    crate::core::env::init_variables(cx, env);
    let dir = std::env::temp_dir().join(format!("owner-autoload-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, call) in [("owner-chain-direct", "(owner-chain-direct)"), ("owner-chain-funcall", "(funcall 'owner-chain-funcall)"), ("owner-chain-apply", "(apply 'owner-chain-apply nil)")] {
        let first = dir.join(format!("{name}-first.el"));
        let last = dir.join(format!("{name}-last.el"));
        std::fs::write(&last, format!("(fset '{name} '(lambda () 23))")).unwrap();
        std::fs::write(&first, format!("(fset '{name} '(autoload {:?} nil nil nil))", last.to_str().unwrap())).unwrap();
        let metadata = list![sym::AUTOLOAD, first.to_str().unwrap(); cx];
        fset_owner(crate::core::env::intern(name, cx), metadata, env, cx).unwrap();
        let form = crate::reader::read(call, cx).unwrap().0; root!(form, cx);
        assert_eq!(crate::interpreter::eval(form, None, env, cx).unwrap(), Object::from(23_i64));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn raw_lambda_dynamic_formals_and_let_are_visible_to_other_functions() {
    crate::interpreter::assert_lisp(r#"(progn
      (setq owner-dynamic-x 1)
      (fset 'owner-dynamic-reader '(lambda () owner-dynamic-x))
      (list (funcall '(lambda (owner-dynamic-x &optional y &rest z)
                       (list (owner-dynamic-reader) y z
                         (let ((owner-dynamic-x 9)) (owner-dynamic-reader))
                         (let* ((owner-dynamic-x 10) (v (owner-dynamic-reader))) v))) 7 8 11 12)
            owner-dynamic-x))"#, "((7 8 (11 12) 9 10) 1)");
}
#[test]
fn raw_lambda_function_does_not_capture_dynamic_or_caller_lexical_cells() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-free-x 1)
      (let ((owner-free-x 99))
        (let ((returned (funcall '(lambda (owner-free-x) (function (lambda () owner-free-x))) 7)))
          (list (functionp returned) (funcall returned)))))"#, "(t 1)");
}
#[test]
fn raw_lambda_preserves_definition_identity_and_mutation_visibility() {
    crate::interpreter::assert_lisp(r#"(let ((raw '(lambda () 3)))
      (fset 'owner-raw-mutable raw)
      (setcar (cdr (cdr raw)) 12)
      (list (eq raw (symbol-function 'owner-raw-mutable)) (owner-raw-mutable)
            (funcall 'owner-raw-mutable) (apply 'owner-raw-mutable nil)))"#, "(t 12 12 12)");
}
#[test]
fn raw_lambda_restores_dynamic_bindings_on_throw_error_and_partial_bind_failure() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-unwind-x 1)
      (list (catch 'done (funcall '(lambda (owner-unwind-x) (garbage-collect) (throw 'done owner-unwind-x)) 7))
        owner-unwind-x
        (condition-case nil (funcall '(lambda (owner-unwind-x) (let ((owner-unwind-x 8)) (error "stop"))) 6) (error 'caught))
        owner-unwind-x
        (condition-case nil (funcall '(lambda (owner-unwind-x t) 0) 9 10) (error 'partial))
        owner-unwind-x))"#, "(7 1 caught 1 partial 1)");
}
#[test]
fn raw_lambda_arity_rejects_before_any_parameter_binding() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-arity-x 5)
      (list (condition-case nil (funcall '(lambda (owner-arity-x y) 0) 9) (error 'missing))
            (condition-case nil (funcall '(lambda (owner-arity-x) 0) 9 10) (error 'extra))
            owner-arity-x))"#, "(missing extra 5)");
}
#[test]
fn lexical_closure_path_still_captures_cells_while_raw_calls_are_dynamic() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-mixed-x 1)
      (let ((owner-mixed-x 9))
        (let ((lexical (function (lambda () owner-mixed-x))) (raw '(lambda () owner-mixed-x)))
          (garbage-collect)
          (list (funcall lexical) (funcall raw)))))"#, "(9 1)");
}
#[test]
fn raw_lambda_condition_handler_binding_is_dynamic_and_unwinds() {
    crate::interpreter::assert_lisp(r#"(progn
      (setq owner-handler-error 'outside)
      (fset 'owner-handler-reader '(lambda () owner-handler-error))
      (list
        (funcall '(lambda () (condition-case owner-handler-error (signal 'error '(detail))
          (error (garbage-collect) (eq owner-handler-error (owner-handler-reader))))))
        owner-handler-error
        (catch 'handler-done (funcall '(lambda ()
          (condition-case owner-handler-error (signal 'error '(detail))
            (error (throw 'handler-done (car (owner-handler-reader))))))))
        owner-handler-error
        (condition-case nil (funcall '(lambda ()
          (condition-case owner-handler-error (signal 'error '(detail))
            (error (signal 'error '(handler-failure)))))) (error 'caught))
        owner-handler-error))"#, "(t outside error outside caught outside)");
}
#[test]
fn raw_lambda_rejects_malformed_and_cyclic_formals_before_binding() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-formal-x 5)
      (list
        (condition-case nil (funcall '(lambda (&rest) 0)) (error 'dangling))
        (condition-case nil (funcall '(lambda (&optional owner-formal-x &optional y) 0) 9) (error 'repeated))
        (condition-case nil (funcall '(lambda (&rest &optional) 0)) (error 'marker))
        (let ((args (list 'owner-formal-x))) (setcdr args args)
          (condition-case nil (funcall (list 'lambda args 0) 9) (error 'cycle)))
        owner-formal-x))"#, "(dangling repeated marker cycle 5)");
}
#[test]
fn raw_lambda_rest_and_heap_return_survive_moving_gc_and_unbound_restoration() {
    crate::interpreter::assert_lisp(r#"(progn
      (makunbound 'owner-temporary-formal)
      (let* ((heap (cons 'kept nil))
             (returned (funcall '(lambda (owner-temporary-formal &rest payload)
                                  (garbage-collect) (garbage-collect)
                                  (cons owner-temporary-formal payload)) heap heap)))
        (garbage-collect)
        (list (eq heap (car returned)) (eq heap (car (cdr returned)))
              (boundp 'owner-temporary-formal))))"#, "(t t nil)");
}

#[test]
fn lisp_eval_nil_is_dynamic_but_explicit_true_is_lexical() {
    crate::interpreter::assert_lisp("(progn (setq owner-mode-x 1) (fset 'owner-mode-reader '(lambda () owner-mode-x)) (list (funcall '(lambda () (eval '(let ((owner-mode-x 9)) (owner-mode-reader))))) (eval '(let ((owner-mode-x 9)) (owner-mode-reader)) t) owner-mode-x))", "(9 1 1)");
}
#[test]
fn macro_observes_callers_binding_mode_and_restores_it() {
    crate::interpreter::assert_lisp("(progn (setq lexical-binding t) (fset 'owner-mode-macro '(macro lambda () (list 'quote lexical-binding))) (list (funcall '(lambda () (owner-mode-macro))) (owner-mode-macro) lexical-binding))", "(nil t t)");
}
#[test]
fn dynamic_function_evaluates_documentation_without_capturing_cells() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-doc-seen nil owner-doc-x 1) (let ((f (funcall '(lambda (owner-doc-x) (function (lambda () (:documentation (progn (setq owner-doc-seen t) "doc")) owner-doc-x))) 9))) (list owner-doc-seen (funcall f))))"#, "(t 1)");
}

#[test]
fn documentation_only_function_has_nil_body() {
    crate::interpreter::assert_lisp(r#"(list (funcall (eval '(function (lambda () (:documentation "doc"))))) (funcall (function (lambda () (:documentation "lexdoc")))) )"#, "(nil nil)");
}

#[test]
fn computed_documentation_data_is_never_executed() {
    crate::interpreter::assert_lisp(r#"(progn (setq owner-doc-data-seen nil) (list (condition-case nil (eval '(function (lambda () (:documentation '(setq owner-doc-data-seen 'bad)) 1))) (error 'unsupported)) owner-doc-data-seen))"#, "(unsupported nil)");
}
