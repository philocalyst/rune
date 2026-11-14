//! Owner-local real obarrays used by the original obarray.el/abbrev.el.
use crate::core::{
    env::{Env, sym},
    gc::{Context, Rt, Rto},
    object::{Function, LispObarray, NIL, Object, ObjectType, Symbol},
};
use anyhow::{Result, ensure};
use rune_core::macros::{call, list, root};
use rune_macros::defun;
defsym!(OBARRAYP);
defsym!(WHOLENUMP);
const MAX_NAME_BYTES: usize = 65536;
const MAX_SNAPSHOT: usize = 65536;
fn type_error(predicate: Symbol, value: Object, env: &mut Rt<Env>, cx: &Context) -> anyhow::Error {
    env.initialize_condition_metadata(cx);
    let data = list![predicate,value;cx];
    crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into()
}
fn resolve<'a>(
    value: Object<'a>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Option<&'a LispObarray>> {
    match value.untag() {
        ObjectType::NIL => Ok(None),
        ObjectType::Obarray(table) => {
            table.validate(cx)?;
            Ok(Some(table))
        }
        _ => Err(type_error(sym::OBARRAYP, value, env, cx)),
    }
}
fn string_name(value: Object, env: &mut Rt<Env>, cx: &Context) -> Result<String> {
    let bytes: &[u8] = match value.untag() {
        ObjectType::String(text) => {
            ensure!(!text.has_properties(), "property-bearing symbol names are unsupported");
            text.as_bytes()
        },
        ObjectType::ByteString(bytes) => {
            ensure!(!bytes.has_properties(), "property-bearing symbol names are unsupported");
            ensure!(bytes.len() <= MAX_NAME_BYTES, "obarray name byte limit exceeded");
            ensure!(bytes.is_ascii(), "non-ASCII unibyte symbol names are unsupported");
            &bytes
        }
        _ => return Err(type_error(sym::STRINGP, value, env, cx)),
    };
    ensure!(bytes.len() <= MAX_NAME_BYTES, "obarray name byte limit exceeded");
    Ok(std::str::from_utf8(bytes)?.to_owned())
}
#[defun]
fn obarray_make<'a>(
    size: Option<Object>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let size = size.unwrap_or(NIL);
    let capacity = match size.untag() {
        ObjectType::NIL => 16,
        ObjectType::Int(n) if n >= 0 => usize::try_from(n)?,
        _ => return Err(type_error(sym::WHOLENUMP, size, env, cx)),
    };
    if capacity > MAX_SNAPSHOT {
        env.initialize_condition_metadata(cx);
        // GNU's excessive fixnum SIZE condition cdr is the original integer,
        // not a one-element list. Our native capacity subset is smaller.
        return Err(crate::eval::EvalError::signal(sym::ARGS_OUT_OF_RANGE.into(),size,cx).into());
    }
    // SIZE is only a capacity hint; storage remains bounded and grows on demand.
    Ok(LispObarray::new(cx, capacity)?.into())
}
#[defun]
fn obarrayp(value: Object) -> bool {
    matches!(value.untag(), ObjectType::Obarray(_))
}

pub(crate) fn intern<'a>(
    string: Object,
    obarray: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Symbol<'a>> {
    // GNU validates the obarray before the name, including doubly-invalid calls.
    let table = resolve(obarray.unwrap_or(NIL), env, cx)?;
    let name = string_name(string, env, cx)?;
    match table {
        Some(table) => table.intern(&name, cx),
        None => Ok(crate::core::env::intern(&name, cx)),
    }
}
pub(crate) fn intern_soft<'a>(
    string: Object<'a>,
    obarray: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Symbol<'a>> {
    let table = resolve(obarray.unwrap_or(NIL), env, cx)?;
    let (name, expected) = match string.untag() {
        ObjectType::NIL => ("nil".to_owned(), Some(sym::NIL)),
        ObjectType::Symbol(symbol) => {
            ensure!(symbol.name().len() <= MAX_NAME_BYTES, "obarray name byte limit exceeded");
            (symbol.name().to_owned(), Some(symbol))
        }
        _ => (string_name(string, env, cx)?, None),
    };
    let found = match table {
        Some(table) => table.get(&name, cx)?,
        None => {
            let map = crate::core::env::INTERNED_SYMBOLS.lock().unwrap();
            map.get(&name).map(|symbol| cx.bind(symbol))
        }
    };
    Ok(found
        .filter(|symbol| expected.is_none_or(|expected| *symbol == expected))
        .unwrap_or(sym::NIL))
}
#[defun]
fn unintern(
    string: Object,
    obarray: Option<Object>,
    env: &mut Rt<Env>,
    cx: &Context,
) -> Result<bool> {
    let table = resolve(obarray.unwrap_or(NIL), env, cx)?;
    let (name, expected) = match string.untag() {
        ObjectType::NIL => ("nil".to_owned(), Some(sym::NIL)),
        ObjectType::Symbol(symbol) => {
            ensure!(symbol.name().len() <= MAX_NAME_BYTES, "obarray name byte limit exceeded");
            (symbol.name().to_owned(), Some(symbol))
        }
        _ => (string_name(string, env, cx)?, None),
    };
    let Some(table) = table else {
        anyhow::bail!("removal from the global symbol namespace is unsupported")
    };
    table.remove(&name, expected, cx)
}
#[defun]
fn mapatoms(
    function: &Rto<Object>,
    obarray: Option<&Rto<Object>>,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<()> {
    let value = obarray.map_or(NIL, |value| value.bind(cx));
    let table = resolve(value, env, cx)?;
    let symbols: Vec<Object> = match table {
        Some(table) => table.snapshot(cx, MAX_SNAPSHOT)?.into_iter().map(Object::from).collect(),
        None => {
            let map = crate::core::env::INTERNED_SYMBOLS.lock().unwrap();
            let mut result = Vec::new();
            for (_, symbol) in map.iter().take(MAX_SNAPSHOT + 1) {
                ensure!(result.len() < MAX_SNAPSHOT, "obarray traversal limit exceeded");
                result.try_reserve(1)?;
                result.push(Object::from(cx.bind(symbol)));
            }
            result
        }
    };
    let len = symbols.len();
    root!(snapshot, cx.add(symbols), cx);
    for index in 0..len {
        env.poll_quit(cx)?;
        let ObjectType::Vec(values) = snapshot.bind(cx).untag() else { unreachable!() };
        root!(symbol, values[index].get(), cx);
        // Removed entries are skipped; new entries are outside this bounded
        // initial snapshot. Never hold a native table borrow during callbacks.
        if let Some(value) = obarray {
            let table = resolve(value.bind(cx), env, cx)?;
            if let Some(table) = table {
                let ObjectType::Symbol(expected) = symbol.bind(cx).untag() else { unreachable!() };
                if table.get(expected.name(), cx)? != Some(expected) {
                    continue;
                }
            }
        }
        if !matches!(
            function.bind(cx).untag(),
            ObjectType::NIL
                | ObjectType::Symbol(_)
                | ObjectType::Cons(_)
                | ObjectType::SubrFn(_)
                | ObjectType::ByteFn(_)
        ) {
            env.initialize_condition_metadata(cx);
            let data = list![function.bind(cx);cx];
            return Err(
                crate::eval::EvalError::signal(sym::INVALID_FUNCTION.into(), data, cx).into()
            );
        }
        let callable: Function = function.bind(cx).try_into().expect("preflighted function tag");
        root!(callable, cx);
        call!(callable,symbol.bind(cx);env,cx)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::interpreter::assert_lisp;
    #[test]
    #[ignore = "real normal bootstrap with pinned GNU30 obarray source; run explicitly in isolation"]
    fn real_bootstrap_preserves_native_obarray_and_official_helpers() {
        crate::editor_document::with_lisp_runtime_owner(80,24,|mut host| {
            host.bootstrap_runtime().map_err(anyhow::Error::msg)?;
            host.evaluate_source("(let* ((a (obarray-make)) (s (intern \"synthetic-bootstrap\" a))) (if (and (obarrayp a) (not (vectorp a)) (= obarray-default-size 4) (featurep 'obarray) (eq s (obarray-get a \"synthetic-bootstrap\"))) nil (error \"pinned obarray source/native authority mismatch\")))").map_err(anyhow::Error::msg)?;
            Ok(())
        }).unwrap();
    }
    #[test]
    fn real_local_obarray_identity_membership_and_removal() {
        assert_lisp(
            "(let ((a (obarray-make)) (b (obarray-make))) (let ((s (intern \"synthetic\" a))) (set s 17) (put s 'tag 19) (list (obarrayp a) (vectorp a) (eq a b) (eq s (intern \"synthetic\" a)) (eq s (intern \"synthetic\" b)) (eq s (intern-soft s a)) (intern-soft (make-symbol \"synthetic\") a) (unintern (make-symbol \"synthetic\") a) (unintern s a) (symbol-value s) (get s 'tag) (eq s (intern \"synthetic\" a)))))",
            "(t nil nil t nil t nil nil t 17 19 nil)",
        );
        assert_lisp(
            "(let ((a (obarray-make))) (let ((s (intern \"nil\" a)) (v (intern \"t\" a))) (set s 7) (set v 8) (list (eq s nil) (eq v t) (symbol-value s) (symbol-value v))))",
            "(nil nil 7 8)",
        );
    }
    #[test]
    fn size_and_name_representation_fail_closed() {
        assert_lisp("(list (obarrayp (obarray-make 0)) (obarrayp (obarray-make 1)) (condition-case e (obarray-make -1) (wrong-type-argument (car (cdr e)))) (condition-case e (obarray-make (expt 2 100)) (wrong-type-argument (car (cdr e)))) (let ((n most-positive-fixnum)) (condition-case e (obarray-make n) (args-out-of-range (eq (cdr e) n)))))", "(t t wholenump wholenump t)");
    }
    #[test]
    fn obarray_callback_snapshot_is_rooted_and_lazy() {
        assert_lisp(
            "(let ((a (obarray-make)) out) (intern \"one\" a) (intern \"two\" a) (mapatoms #'(lambda (s) (garbage-collect) (setq out (cons s out))) a) (list (length out) (eq (intern-soft (car out) a) (car out)) (eq (intern-soft (car (cdr out)) a) (car (cdr out)))))",
            "(2 t t)",
        );
        assert_lisp("(mapatoms 7 (obarray-make))", "nil");
        assert_lisp("(let ((a (obarray-make)) (x (vector nil))) (aset x 0 x) (intern \"x\" a) (condition-case e (mapatoms x a) (invalid-function (eq (car (cdr e)) x))))", "t");
        assert_lisp(
            "(let ((a (obarray-make))) (intern \"x\" a) (condition-case e (mapatoms 7 a) (invalid-function (car (cdr e)))))",
            "7",
        );
    }
    #[test]
    fn unsupported_name_metadata_and_bytes_are_not_silently_coerced() {
        assert_lisp("(let ((a (obarray-make)) (name (propertize \"annotated\" 'p 7))) (condition-case nil (progn (intern name a) nil) (error (eq nil (intern-soft \"annotated\" a)))))", "t");
        assert_lisp("(let ((a (obarray-make))) (condition-case nil (progn (intern (unibyte-string 255) a) nil) (error t)))", "t");
    }
    #[test]
    fn obarray_validation_precedes_name_without_cyclic_rendering() {
        assert_lisp(
            "(let ((x (vector nil))) (aset x 0 x) (condition-case e (intern x x) (wrong-type-argument (list (car (cdr e)) (eq x (car (cdr (cdr e))))))))",
            "(obarrayp t)",
        );
        assert_lisp(
            "(let ((x (vector nil))) (aset x 0 x) (condition-case e (intern x (obarray-make)) (wrong-type-argument (list (car (cdr e)) (eq x (car (cdr (cdr e))))))))",
            "(stringp t)",
        );
    }
}
