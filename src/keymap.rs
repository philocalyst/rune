//! Sparse Lisp keymaps. All cells remain ordinary traced Lisp objects.
use crate::{
    core::{
        cons::Cons,
        env::{Env, sym},
        gc::{Context, Rt},
        object::{NIL, Object, ObjectType},
    },
    fns,
};
use anyhow::{Result, anyhow, bail, ensure};
use rune_macros::defun;

defsym!(KEYMAP);
defsym!(GLOBAL_MAP);
// GNU Emacs 30.1 src/keymap.c::syms_of_keymap creates this real sparse
// map before Lisp subr.el adds bindings. Unlike global-map it has no Lisp
// initializer before minibuffer-local-password-map inherits it.
defvar!(MINIBUFFER_LOCAL_MAP, crate::keymap::make_sparse_keymap(None, cx));

#[defun]
pub(crate) fn make_sparse_keymap<'a>(string: Option<&str>, cx: &'a Context) -> Object<'a> {
    let tail = string.map(|s| Cons::new(s, NIL, cx).into()).unwrap_or(NIL);
    Cons::new(sym::KEYMAP, tail, cx).into()
}
#[defun]
fn make_keymap<'a>(string: Option<&str>, cx: &'a Context) -> Object<'a> {
    // Sparse storage has the same lookup semantics for the supported events.
    make_sparse_keymap(string, cx)
}
pub(crate) fn direct_map(object: Object) -> bool {
    object.cons().is_some_and(|c| c.car().ptr_eq(Object::from(sym::KEYMAP)))
}
fn map_object<'a>(
    mut object: Object<'a>,
    env: &Rt<Env>,
    cx: &'a Context,
) -> Result<Option<Object<'a>>> {
    let mut seen = Vec::new();
    for _ in 0..256 {
        if direct_map(object) {
            return Ok(Some(object));
        }
        let ObjectType::Symbol(symbol) = object.untag() else { return Ok(None) };
        if object.is_nil() {
            return Ok(None);
        }
        ensure!(
            !seen.iter().any(|x: &Object| x.ptr_eq(object)),
            "cyclic keymap symbol indirection"
        );
        seen.push(object);
        object = crate::data::symbol_function_owner(symbol, env, cx);
    }
    bail!("keymap symbol indirection too deep")
}
#[defun]
fn keymapp(object: Object, env: &Rt<Env>, cx: &Context) -> Result<bool> {
    Ok(map_object(object, env, cx)?.is_some())
}
fn checked<'a>(map: Object<'a>, env: &Rt<Env>, cx: &'a Context) -> Result<&'a Cons> {
    map_object(map, env, cx)?
        .and_then(|m| m.cons())
        .ok_or_else(|| anyhow!("expected keymap"))
}
fn parent<'a>(map: Object<'a>, env: &Rt<Env>, cx: &'a Context) -> Result<Object<'a>> {
    let mut tail = checked(map, env, cx)?.cdr();
    for _ in 0..65536 {
        if tail.is_nil() {
            return Ok(NIL);
        }
        if direct_map(tail) {
            return Ok(tail);
        }
        tail = tail.cons().ok_or_else(|| anyhow!("malformed keymap tail"))?.cdr();
    }
    bail!("circular or oversized keymap")
}
#[defun]
fn keymap_parent<'a>(map: Object<'a>, env: &Rt<Env>, cx: &'a Context) -> Result<Object<'a>> {
    parent(map, env, cx)
}
#[defun]
fn set_keymap_parent<'a>(
    map: Object<'a>,
    new_parent: Object<'a>,
    env: &Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let map = map_object(map, env, cx)?.ok_or_else(|| anyhow!("expected keymap"))?;
    let new_parent = if new_parent.is_nil() {
        NIL
    } else {
        map_object(new_parent, env, cx)?.ok_or_else(|| anyhow!("expected parent keymap"))?
    };
    let mut ancestor = new_parent;
    for depth in 0..=256 {
        ensure!(!ancestor.ptr_eq(map), "cyclic keymap parent");
        if ancestor.is_nil() {
            break;
        }
        ensure!(depth < 256, "keymap inheritance too deep");
        ancestor = parent(ancestor, env, cx)?;
    }
    let mut cell = checked(map, env, cx)?;
    for _ in 0..65536 {
        let tail = cell.cdr();
        if tail.is_nil() || direct_map(tail) {
            cell.set_cdr(new_parent)?;
            return Ok(new_parent);
        }
        cell = tail.cons().ok_or_else(|| anyhow!("malformed keymap tail"))?;
    }
    bail!("circular or oversized keymap")
}
fn local_binding<'a>(
    map: Object<'a>,
    event: Object<'a>,
    env: &Rt<Env>,
    cx: &'a Context,
) -> Result<Option<Object<'a>>> {
    let mut tail = checked(map, env, cx)?.cdr();
    for _ in 0..65536 {
        if tail.is_nil() || direct_map(tail) {
            return Ok(None);
        }
        let c = tail.cons().ok_or_else(|| anyhow!("malformed keymap tail"))?;
        if let Some(binding) = c.car().cons() {
            if fns::eql(binding.car(), event) {
                return Ok(Some(binding.cdr()));
            }
        }
        tail = c.cdr();
    }
    bail!("circular or oversized keymap")
}
fn events<'a>(key: Object<'a>, cx: &'a Context) -> Result<Vec<Object<'a>>> {
    match key.untag() {
        ObjectType::Vec(v) => Ok(v.iter().map(|x| x.get()).collect()),
        ObjectType::String(s) => Ok(s.chars().map(|c| Object::from(c as i64)).collect()),
        ObjectType::ByteString(s) => Ok(s
            .iter()
            .map(|c| {
                Object::from(if *c >= 128 {
                    i64::from(*c & 127) | (1 << 27)
                } else {
                    i64::from(*c)
                })
            })
            .collect()),
        _ => {
            let _ = cx;
            bail!("key sequence must be a string or vector")
        }
    }
}
#[defun]
fn lookup_key<'a>(
    map: Object<'a>,
    key: Object<'a>,
    accept_default: Option<Object<'a>>,
    env: &Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    checked(map, env, cx)?;
    let keys = events(key, cx)?;
    if keys.is_empty() {
        return Ok(map);
    }
    let mut maps = vec![map];
    for (i, event) in keys.iter().copied().enumerate() {
        let mut prefixes = Vec::new();
        let mut first = None;
        for candidate in maps {
            let mut current = candidate;
            for depth in 0..=256 {
                if current.is_nil() {
                    break;
                }
                ensure!(depth < 256, "keymap inheritance too deep");
                if let Some(value) = local_binding(current, event, env, cx)?.or(
                    if accept_default.is_some_and(|x| !x.is_nil()) {
                        local_binding(current, crate::core::object::TRUE, env, cx)?
                    } else {
                        None
                    },
                ) {
                    if first.is_none() {
                        first = Some(value);
                    }
                    if keymapp(value, env, cx)? { prefixes.push(value) } else { break }
                }
                current = parent(current, env, cx)?;
            }
        }
        let Some(value) = first else { return Ok(NIL) };
        if !keymapp(value, env, cx)? {
            return Ok(if i + 1 == keys.len() { value } else { ((i + 1) as i64).into() });
        }
        // GNU links corresponding inherited prefix maps lazily during lookup.
        // Keep ordinary local-prefix identity while exposing its real parent.
        if prefixes.len() > 1 && parent(value, env, cx)?.is_nil() && !value.ptr_eq(prefixes[1]) {
            set_keymap_parent(value, prefixes[1], env, cx)?;
        }
        if i + 1 == keys.len() {
            return Ok(value);
        }
        maps = prefixes;
    }
    Ok(NIL)
}
#[defun]
pub(crate) fn define_key<'a>(
    map: Object<'a>,
    key: Object<'a>,
    definition: Object<'a>,
    env: &Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    checked(map, env, cx)?;
    let keys = events(key, cx)?;
    if keys.is_empty() {
        return Ok(NIL);
    }
    let mut current = map;
    for (i, event) in keys.iter().copied().enumerate() {
        let final_key = i + 1 == keys.len();
        let value = if final_key {
            definition
        } else {
            let old = local_binding(current, event, env, cx)?.unwrap_or(NIL);
            if keymapp(old, env, cx)? {
                old
            } else {
                ensure!(old.is_nil(), "key sequence starts with non-prefix key");
                make_sparse_keymap(None, cx)
            }
        };
        let head = checked(current, env, cx)?;
        let mut tail = head.cdr();
        let mut updated = false;
        for _ in 0..65536 {
            if tail.is_nil() || direct_map(tail) {
                break;
            }
            let c = tail.cons().ok_or_else(|| anyhow!("malformed keymap tail"))?;
            if let Some(pair) = c.car().cons() {
                if fns::eql(pair.car(), event) {
                    pair.set_cdr(value)?;
                    updated = true;
                    break;
                }
            }
            tail = c.cdr();
        }
        if !updated {
            let pair = Cons::new(event, value, cx);
            head.set_cdr(Cons::new(pair, head.cdr(), cx).into())?;
        }
        current = value;
    }
    Ok(definition)
}
#[defun]
fn use_global_map(map: Object, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    let map = map_object(map, env, cx)?.ok_or_else(|| anyhow!("expected keymap"))?;
    cancel_native_prefix(env);
    env.vars.insert(sym::GLOBAL_MAP, map);
    Ok(())
}
#[defun]
fn current_global_map<'a>(env: &Rt<Env>, cx: &'a Context) -> Object<'a> {
    env.vars.get(sym::GLOBAL_MAP).map(|v| v.bind(cx)).unwrap_or(NIL)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::assert_lisp;
    struct FunctionCycleCleanup<'a>([crate::core::object::Symbol<'a>; 2]);
    impl Drop for FunctionCycleCleanup<'_> {
        fn drop(&mut self) {
            for symbol in self.0 {
                symbol.unbind_func();
            }
        }
    }
    #[test]
    fn native_startup_preserves_real_lisp_global_initializer_and_real_c_minibuffer_map() {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        assert!(
            env.vars.get(sym::GLOBAL_MAP).is_none(),
            "native nil must not suppress Lisp defvar initializer"
        );
        let mini = env.vars.get(sym::MINIBUFFER_LOCAL_MAP).unwrap().bind(cx);
        root!(mini, cx);
        assert!(keymapp(mini.bind(cx), env, cx).unwrap());
        let key = cx.add("x");
        let command = crate::lread::intern("startup-map-command", cx);
        define_key(mini.bind(cx), key, command.into(), env, cx).unwrap();
        assert!(
            lookup_key(mini.bind(cx), key, None, env, cx)
                .unwrap()
                .ptr_eq(Object::from(command))
        );
        crate::lread::load_internal("(setq global-startup-initializer-ran nil) (defvar global-map (progn (setq global-startup-initializer-ran t) (make-keymap))) (use-global-map global-map) (if global-startup-initializer-ran nil (error \"skipped actual Lisp initializer\"))",cx,env).unwrap();
        assert!(keymapp(current_global_map(env, cx), env, cx).unwrap());
    }
    #[test]
    fn sparse_prefix_lookup_and_inherited_prefixes_match_gnu() {
        for (source, expected) in [
            (
                "(let ((m (make-sparse-keymap))) (list (keymapp m) (lookup-key m \"\") (define-key m \"\" 'ignored)))",
                "(t (keymap) nil)",
            ),
            (
                "(let ((m (make-sparse-keymap))) (define-key m \"ab\" 'forward-char) (list (lookup-key m \"ab\") (lookup-key m \"abc\") (keymapp (lookup-key m \"a\"))))",
                "(forward-char 2 t)",
            ),
            (
                "(let ((p (make-sparse-keymap)) (m (make-sparse-keymap))) (define-key p \"ab\" 'old) (set-keymap-parent m p) (define-key m \"ac\" 'new) (list (lookup-key m \"ab\") (lookup-key m \"ac\") (lookup-key p \"ac\") (keymap-parent (lookup-key m \"a\"))))",
                "(old new nil (keymap (98 . old)))",
            ),
            (
                "(let ((p (make-sparse-keymap)) (m (make-sparse-keymap))) (define-key p \"a\" 'old) (set-keymap-parent m p) (define-key m \"a\" nil) (lookup-key m \"a\"))",
                "nil",
            ),
            (
                "(let ((m (make-sparse-keymap))) (define-key m [f5] 'foo) (use-global-map m) (list (eq m (current-global-map)) (lookup-key (current-global-map) [f5])))",
                "(t foo)",
            ),
        ] {
            assert_lisp(source, expected);
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn meta_control_and_default_event_lookup_match_gnu() {
        for source in [
            r#"(let ((m (make-sparse-keymap))) (define-key m "\M-x" 'foo) (list (lookup-key m [134217848]) (lookup-key m "\M-x")))"#,
            r#"(let ((m (make-sparse-keymap))) (define-key m [t] 'fallback) (list (lookup-key m [f7]) (lookup-key m [f7] t)))"#,
        ] {
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn symbol_function_keymap_aliases_and_prefixes_match_gnu() {
        for source in [
            "(let ((p (make-sparse-keymap)) (m (make-sparse-keymap))) (define-key p \"b\" 'foo) (fset 'native-prefix-alias p) (define-key m \"a\" 'native-prefix-alias) (list (keymapp 'native-prefix-alias) (lookup-key m \"a\") (lookup-key m \"ab\")))",
            "(let ((p (make-sparse-keymap)) (m (make-sparse-keymap))) (fset 'native-map-alias p) (fset 'native-map-alias2 'native-map-alias) (define-key 'native-map-alias2 [f5] 'bar) (set-keymap-parent m 'native-map-alias2) (use-global-map 'native-map-alias2) (list (lookup-key m [f5]) (eq (keymap-parent m) p) (eq (current-global-map) p)))",
        ] {
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn cyclic_symbol_keymap_aliases_fail_without_looping() {
        use super::*;
        use crate::core::gc::RootSet;
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        use rune_core::macros::root;
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        let a = crate::lread::intern("native-keymap-cycle-a", cx);
        let b = crate::lread::intern("native-keymap-cycle-b", cx);
        let cleanup = FunctionCycleCleanup([a, b]);
        crate::data::fset(a, b.into()).unwrap();
        crate::data::fset(b, a.into()).unwrap();
        let outcome = keymapp(a.into(), env, cx);
        drop(cleanup);
        assert!(outcome.is_err());
    }
    #[test]
    fn owner_map_identity_replacement_and_failed_replacement_match_gnu() {
        for source in [
            "(let ((m (make-sparse-keymap))) (define-key m [f5] 'foo) (fset 'native-owned-map m) (fset 'native-owned-alias 'native-owned-map) (garbage-collect) (list (eq (fset 'native-owned-map m) m) (eq (symbol-function 'native-owned-map) m) (symbol-function 'native-owned-alias) (fboundp 'native-owned-map) (lookup-key 'native-owned-alias [f5])))",
            "(let ((m (make-sparse-keymap))) (fset 'native-replace-map m) (fset 'native-replace-alias 'native-replace-map) (fset 'native-replace-map (function (lambda () 7))) (list (keymapp 'native-replace-map) (funcall 'native-replace-alias) (eq (symbol-function 'native-replace-alias) 'native-replace-map)))",
        ] {
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn failed_existing_global_backend_replacement_keeps_previous_owner_map() {
        // GNU accepts arbitrary fset values; Rune's existing global backend
        // rejects a non-function integer. This is atomicity, not parity.
        assert_lisp(
            "(let ((m (make-sparse-keymap))) (fset 'native-failed-map m) (condition-case nil (fset 'native-failed-map 42) (error nil)) (list (eq (symbol-function 'native-failed-map) m) (keymapp 'native-failed-map)))",
            "(t t)",
        );
    }
    #[test]
    fn replacing_symbol_prefix_invalidates_pending_owner_prefix_and_keeps_gc_roots() {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::lread::load_internal("(fset 'native-old-command '(lambda () (interactive) (insert \"OLD\"))) (fset 'native-new-command '(lambda () (interactive) (insert \"NEW\"))) (setq native-old-map (make-sparse-keymap) native-new-map (make-sparse-keymap) native-top-map (make-sparse-keymap)) (define-key native-old-map \"y\" 'native-old-command) (define-key native-new-map \"y\" 'native-new-command) (fset 'native-prefix-target native-old-map) (fset 'native-prefix-edge 'native-prefix-target) (define-key native-top-map \"x\" 'native-prefix-edge) (use-global-map native-top-map)",cx,env).unwrap();
        assert_eq!(
            dispatch_native_event(NativeEvent::Character(120), 42, env, cx).unwrap(),
            NativeDispatch::Prefix
        );
        crate::lread::load_internal("(fset 'native-prefix-target native-new-map)", cx, env)
            .unwrap();
        cx.garbage_collect(true);
        assert_eq!(
            dispatch_native_event(NativeEvent::Character(121), 42, env, cx).unwrap(),
            NativeDispatch::Unbound
        );
        assert_eq!(
            dispatch_native_event(NativeEvent::Character(120), 42, env, cx).unwrap(),
            NativeDispatch::Prefix
        );
        assert_eq!(
            dispatch_native_event(NativeEvent::Character(121), 42, env, cx).unwrap(),
            NativeDispatch::Executed
        );
        assert_eq!(env.buffer_read(cx).unwrap().document.text().to_string(), "NEW");
    }
    #[test]
    fn direct_keymap_roots_are_owner_local_and_never_leave_stale_global_callable() {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(a, init(Env::new_isolated(cx).unwrap()), cx);
        root!(b, init(Env::new_isolated(cx).unwrap()), cx);
        crate::lread::load_internal("(fset 'native-owner-only-map (function (lambda () 11))) (setq native-owned-graph (make-sparse-keymap)) (fset 'native-owner-only-map native-owned-graph) (fset 'native-owner-only-edge 'native-owner-only-map)",cx,a).unwrap();
        let symbol = crate::lread::intern("native-owner-only-map", cx);
        root!(symbol, cx);
        let alias = crate::lread::intern("native-owner-only-edge", cx);
        root!(alias, cx);
        assert!(keymapp(symbol.bind(cx).into(), a, cx).unwrap());
        assert!(!keymapp(symbol.bind(cx).into(), b, cx).unwrap());
        assert!(
            crate::data::symbol_function(symbol.bind(cx), cx).is_nil(),
            "old global callable must be retired"
        );
        assert!(crate::data::symbol_function_owner(symbol.bind(cx), b, cx).is_nil());
        assert!(keymapp(alias.bind(cx).into(), a, cx).unwrap());
        assert!(!keymapp(alias.bind(cx).into(), b, cx).unwrap());
        cx.garbage_collect(true);
        assert!(keymapp(symbol.bind(cx).into(), a, cx).unwrap());
        crate::lread::load_internal(
            "(fset 'native-owner-only-map (function (lambda () 22)))",
            cx,
            a,
        )
        .unwrap();
        assert!(!keymapp(symbol.bind(cx).into(), a, cx).unwrap());
        crate::lread::load_internal(
            "(if (= (funcall 'native-owner-only-edge) 22) nil (error \"stale alias\"))",
            cx,
            b,
        )
        .unwrap();
    }
    #[test]
    fn uninterned_keymap_symbol_key_and_map_value_are_traced_across_gc() {
        let source = "(let* ((s (make-symbol \"owner-map\")) (m (make-sparse-keymap))) (define-key m [f5] 'mapped) (fset s m) (garbage-collect) (list (eq (symbol-function s) m) (lookup-key s [f5])))";
        crate::assert_elprop!("{}", source);
    }
    #[test]
    fn unsupported_uninterned_alias_and_invalid_alias_graph_leave_old_binding_unchanged() {
        assert_lisp(
            "(let ((s (make-symbol \"edge-target\")) (m (make-sparse-keymap))) (fset s m) (fset 'native-retained-normal (function (lambda () 8))) (condition-case nil (fset 'native-retained-normal s) (error nil)) (funcall 'native-retained-normal))",
            "8",
        );
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        let a = crate::lread::intern("native-preflight-cycle-a", cx);
        let b = crate::lread::intern("native-preflight-cycle-b", cx);
        let target = crate::lread::intern("native-preflight-target", cx);
        let cleanup = FunctionCycleCleanup([a, b]);
        crate::data::fset(a, b.into()).unwrap();
        crate::data::fset(b, a.into()).unwrap();
        let outcome = preflight_keymap_alias(target, a.into(), env, cx);
        drop(cleanup);
        assert!(outcome.is_err(), "cycle errors must propagate before publication");
    }
    #[test]
    fn explicit_nil_prefix_is_terminal_for_too_long_lookup() {
        let source = "(let ((p (make-sparse-keymap)) (c (make-sparse-keymap))) (define-key p \"a\" 'ignore) (set-keymap-parent c p) (define-key c \"a\" nil) (list (lookup-key c \"a\") (lookup-key c \"ab\")))";
        assert_lisp(source, "(nil 1)");
        crate::assert_elprop!("{}", source);
    }
    #[test]
    fn noarg_interpreted_interactive_command_runs_and_is_gnu_qualified() {
        let source = "(progn (setq ran 0) (fset 'native-test-command '(lambda () (interactive) (setq ran (+ ran 1)))) (list (commandp 'native-test-command) (call-interactively 'native-test-command) ran))";
        assert_lisp(source, "(t 1 1)");
        crate::assert_elprop!("{}", source);
        assert_lisp("(commandp '(lambda () 42))", "nil");
    }
    #[test]
    fn owner_prefix_survives_gc_and_dispatches_printable_exactly_once() {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::lread::load_internal("(setq native-fired 0) (fset 'native-bound '(lambda () (interactive) (setq native-fired (+ native-fired 1)) (insert \"bound\"))) (setq native-map (make-sparse-keymap)) (define-key native-map \"xy\" 'native-bound) (use-global-map native-map)",cx,env).unwrap();
        assert_eq!(
            dispatch_native_event(NativeEvent::Character('x' as u32), 42, env, cx).unwrap(),
            NativeDispatch::Prefix
        );
        cx.garbage_collect(true);
        assert_eq!(
            dispatch_native_event(NativeEvent::Character('y' as u32), 42, env, cx).unwrap(),
            NativeDispatch::Executed
        );
        assert_eq!(env.buffer_read(cx).unwrap().document.text().to_string(), "bound");
        assert_eq!(
            dispatch_native_event(NativeEvent::Character('y' as u32), 42, env, cx).unwrap(),
            NativeDispatch::Unbound
        );
        assert_eq!(
            dispatch_native_event(NativeEvent::Character('x' as u32), 42, env, cx).unwrap(),
            NativeDispatch::Prefix
        );
        assert_eq!(
            dispatch_native_event(NativeEvent::Character('y' as u32), 43, env, cx).unwrap(),
            NativeDispatch::Unbound
        );
    }
    #[test]
    fn keymaps_remain_rooted_through_real_collection() {
        assert_lisp(
            "(let ((m (make-sparse-keymap))) (define-key m [f5] (list 'lambda nil '(insert \"x\"))) (use-global-map m) (garbage-collect) (list (keymapp (current-global-map)) (car (lookup-key (current-global-map) [f5]))))",
            "(t lambda)",
        );
    }
}

fn resolve_command<'a>(mut object: Object<'a>, cx: &'a Context) -> Result<Object<'a>> {
    let mut seen = Vec::new();
    for _ in 0..256 {
        let ObjectType::Symbol(symbol) = object.untag() else { return Ok(object) };
        if object.is_nil() {
            return Ok(NIL);
        }
        ensure!(!seen.iter().any(|x: &Object| x.ptr_eq(object)), "cyclic command indirection");
        seen.push(object);
        object = symbol.func(cx).map(Into::into).unwrap_or(NIL);
    }
    bail!("command indirection too deep")
}
fn interactive_spec<'a>(object: Object<'a>, cx: &'a Context) -> Result<Option<Object<'a>>> {
    let object = resolve_command(object, cx)?;
    let Some(head) = object.cons() else { return Ok(None) };
    let mut body = head.cdr();
    if head.car().ptr_eq(Object::from(sym::CLOSURE)) {
        body = body.cons().ok_or_else(|| anyhow!("malformed closure"))?.cdr();
    } else if !head.car().ptr_eq(Object::from(sym::LAMBDA)) {
        return Ok(None);
    }
    body = body.cons().ok_or_else(|| anyhow!("malformed lambda"))?.cdr();
    for _ in 0..1024 {
        if body.is_nil() {
            return Ok(None);
        }
        let cell = body.cons().ok_or_else(|| anyhow!("malformed function body"))?;
        if let Some(form) = cell.car().cons() {
            if form.car().ptr_eq(Object::from(sym::INTERACTIVE)) {
                return Ok(Some(form.cdr().cons().map(|c| c.car()).unwrap_or(NIL)));
            }
        }
        body = cell.cdr();
    }
    bail!("function body exceeds interactive inspection bound")
}
#[defun]
fn commandp(
    command: Object,
    _for_call_interactively: Option<Object>,
    cx: &Context,
) -> Result<bool> {
    Ok(interactive_spec(command, cx)?.is_some())
}
#[defun]
fn call_interactively<'a>(
    command: &crate::core::gc::Rto<Object>,
    _record_flag: crate::core::object::OptionalFlag,
    keys: Option<&crate::core::gc::Rto<Object>>,
    env: &mut Rt<Env>,
    cx: &'a mut Context,
) -> Result<Object<'a>> {
    use rune_core::macros::{list, root};
    ensure!(
        keys.is_none_or(|x| x.bind(cx).is_nil()),
        "explicit interactive key sequence unsupported"
    );
    let spec = interactive_spec(command.bind(cx), cx)?
        .ok_or_else(|| anyhow!("not an interpreted interactive command"))?;
    ensure!(
        spec.is_nil()
            || matches!(spec.untag(),ObjectType::String(s) if s.is_empty())
            || matches!(spec.untag(),ObjectType::ByteString(s) if s.is_empty()),
        "interactive arguments unsupported; only no-argument commands are supported"
    );
    let resolved = resolve_command(command.bind(cx), cx)?;
    let quoted = if resolved.cons().is_some_and(|c| c.car().ptr_eq(Object::from(sym::LAMBDA))) {
        list![sym::FUNCTION,resolved;cx]
    } else {
        list![sym::QUOTE,command.bind(cx);cx]
    };
    let form = list![sym::FUNCALL,quoted;cx];
    root!(form, cx);
    crate::interpreter::eval(form, None, env, cx).map_err(Into::into)
}

/// Owner-only result. No Lisp value or prefix escapes to native UI.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NativeDispatch {
    Unbound,
    Prefix,
    Executed,
}
#[derive(Debug)]
pub(crate) enum NativeEvent<'a> {
    Character(u32),
    Named(&'a str),
}
pub(crate) fn cancel_native_prefix(env: &mut Rt<Env>) {
    env.native_key_prefix.set(NIL);
    env.native_key_sequence = None;
}
pub(crate) fn dispatch_native_event(
    event: NativeEvent<'_>,
    sequence: u64,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<NativeDispatch> {
    use rune_core::macros::root;
    if env.native_key_sequence != Some(sequence) {
        cancel_native_prefix(env)
    }
    let pending = !env.native_key_prefix.bind(cx).is_nil();
    let map = if pending {
        env.native_key_prefix.bind(cx)
    } else {
        current_global_map(env, cx)
    };
    if map.is_nil() {
        return Ok(NativeDispatch::Unbound);
    }
    root!(map, cx);
    let event: Object = match event {
        NativeEvent::Character(c) => i64::from(c).into(),
        NativeEvent::Named(name) => crate::lread::intern(name, cx).into(),
    };
    let key = cx.add(vec![event]);
    let definition = lookup_key(map.bind(cx), key, None, env, cx)?;
    root!(definition, cx);
    if keymapp(definition.bind(cx), env, cx)? {
        env.native_key_prefix.set(definition.bind(cx));
        env.native_key_sequence = Some(sequence);
        return Ok(NativeDispatch::Prefix);
    }
    cancel_native_prefix(env);
    if definition.bind(cx).is_nil() {
        ensure!(!pending, "undefined key after prefix");
        return Ok(NativeDispatch::Unbound);
    }
    call_interactively(definition, None, None, env, cx)?;
    Ok(NativeDispatch::Executed)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EncodedNativeKey {
    Character(u32),
    Named(String),
}
pub(crate) fn encode_native_key(
    key: &str,
    control: bool,
    meta: bool,
    shift: bool,
) -> EncodedNativeKey {
    let mut chars = key.chars();
    let first = chars.next();
    let character = if key == "space" {
        Some(' ')
    } else {
        first.filter(|_| chars.next().is_none())
    };
    if let Some(c) = character {
        let mut code = c as u32;
        if control {
            code = match c {
                'a'..='z' => u32::from(c) - u32::from('a') + 1,
                'A'..='Z' => (u32::from(c) - u32::from('A') + 1) | (1 << 25),
                '@' | '['..='_' => u32::from(c) & 31,
                c if u32::from(c) < 32 => u32::from(c),
                _ => code | (1 << 26),
            };
        }
        if shift {
            if !control && c.is_ascii_lowercase() {
                code = u32::from(c.to_ascii_uppercase());
            } else {
                code |= 1 << 25;
            }
        }
        if meta {
            code |= 1 << 27;
        }
        EncodedNativeKey::Character(code)
    } else if !control && !meta && !shift && matches!(key, "enter" | "tab" | "backspace") {
        EncodedNativeKey::Character(match key {
            "enter" => 13,
            "tab" => 9,
            _ => 127,
        })
    } else {
        EncodedNativeKey::Named(format!(
            "{}{}{}{}",
            if control { "C-" } else { "" },
            if meta { "M-" } else { "" },
            if shift { "S-" } else { "" },
            if key == "enter" { "return" } else { key }
        ))
    }
}
#[cfg(test)]
mod native_encoding_tests {
    use super::*;
    #[test]
    fn all_ascii_modifier_combinations_match_gnu_event_convert_list() {
        let mut values = Vec::new();
        for c in 0..128u32 {
            for flags in 0..8u32 {
                let EncodedNativeKey::Character(code) = encode_native_key(
                    &char::from_u32(c).unwrap().to_string(),
                    flags & 1 != 0,
                    flags & 2 != 0,
                    flags & 4 != 0,
                ) else {
                    panic!("character encoding")
                };
                values.push(code.to_string());
            }
        }
        crate::library::elprop::start_emacs().unwrap();
        let gnu=crate::library::elprop::eval("(let (out) (dotimes (c 128) (dotimes (f 8) (push (event-convert-list (append (if (/= 0 (logand f 1)) '(control)) (if (/= 0 (logand f 2)) '(meta)) (if (/= 0 (logand f 4)) '(shift)) (list c))) out))) (nreverse out))").unwrap();
        assert_eq!(format!("({})", values.join(" ")), gnu);
        assert_eq!(
            encode_native_key("λ", false, true, true),
            EncodedNativeKey::Character(167773115)
        );
    }
    #[test]
    fn controls_do_not_collide_with_digits_punctuation_or_shift_and_match_gnu() {
        let cases = [
            ("1", true, false, false, 67108913),
            ("q", true, false, false, 17),
            (".", true, false, false, 67108910),
            ("a", true, false, true, 33554433),
            ("?", true, false, false, 67108927),
            ("[", true, false, false, 27),
            ("x", false, true, false, 134217848),
        ];
        for (key, c, m, s, expected) in cases {
            assert_eq!(encode_native_key(key, c, m, s), EncodedNativeKey::Character(expected));
        }
        crate::library::elprop::start_emacs().unwrap();
        let gnu=crate::library::elprop::eval(r#"(mapcar (lambda (s) (aref (kbd s) 0)) '("C-1" "C-q" "C-." "C-S-a" "C-?" "C-[" "M-x"))"#).unwrap();
        assert_eq!(gnu, "(67108913 17 67108910 33554433 67108927 27 134217848)");
    }
}

/// Global edges contain symbols only. Preflight before replacing a direct
/// owner map so an alias cycle cannot destroy the last valid definition.
pub(crate) fn preflight_keymap_alias(
    symbol: crate::core::object::Symbol,
    definition: Object,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<()> {
    if !definition.is_symbol() || definition.is_nil() || map_object(definition, env, cx)?.is_none()
    {
        return Ok(());
    }
    if let ObjectType::Symbol(target) = definition.untag() {
        ensure!(
            target.interned(),
            "uninterned keymap alias edges unsupported; direct owner maps remain supported"
        );
    }
    let mut current = definition;
    let mut seen = Vec::new();
    for _ in 0..256 {
        let ObjectType::Symbol(s) = current.untag() else { return Ok(()) };
        if current.is_nil() {
            return Ok(());
        }
        ensure!(s != symbol, "cyclic keymap symbol replacement");
        ensure!(!seen.iter().any(|x: &Object| x.ptr_eq(current)), "cyclic keymap alias");
        seen.push(current);
        current = crate::data::symbol_function_owner(s, env, cx);
    }
    bail!("keymap symbol indirection too deep")
}
