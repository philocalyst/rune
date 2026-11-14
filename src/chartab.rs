use crate::core::object::{CharTable, CharTableInner, Object, Symbol};
use anyhow::Result;
use rune_macros::defun;

#[defun]
fn make_char_table<'ob>(
    purpose: Symbol<'ob>,
    init: Option<Object<'ob>>,
    env: &crate::core::gc::Rt<crate::core::env::Env>,
    cx: &crate::core::gc::Context,
) -> Result<CharTableInner<'ob>> {
    // Extra-slot subtypes require representation not yet implemented here.
    let extra = crate::data::get(purpose, crate::core::env::sym::CHAR_TABLE_EXTRA_SLOTS, env, cx);
    anyhow::ensure!(matches!(extra.untag(), crate::core::object::ObjectType::NIL | crate::core::object::ObjectType::Int(0)),
        "char-table extra slots are unsupported");
    Ok(CharTableInner::new(init).with_purpose(purpose.into()))
}

#[defun]
fn set_char_table_parent<'ob>(
    table: &'ob CharTable,
    parent: Option<&'ob CharTable>,
) -> Result<Option<&'ob CharTable>> {
    table.validate_parent(parent)?;
    table.set_parent(parent)?;
    Ok(parent)
}

// Predicate symbol supplies structured errors without formatting arbitrary graphs.
defsym!(CHAR_TABLE_P);

#[defun]
fn map_char_table(
    function: &crate::core::gc::Rto<Object>,
    table: &crate::core::gc::Rto<Object>,
    env: &mut crate::core::gc::Rt<crate::core::env::Env>,
    cx: &mut crate::core::gc::Context,
) -> Result<()> {
    use crate::core::{
        env::sym,
        object::{Function, ObjectType},
    };
    use rune_core::macros::{call, list, root};
    if !matches!(table.bind(cx).untag(), ObjectType::CharTable(_)) {
        env.initialize_condition_metadata(cx);
        let data = list![sym::CHAR_TABLE_P, table.bind(cx); cx];
        return Err(
            crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into()
        );
    }
    root!(range, Object::from(crate::core::cons::Cons::new(0, 0, cx)), cx);
    let mut start = 0usize;
    let mut remaining = 65536usize;
    loop {
        env.poll_quit(cx)?;
        let ObjectType::CharTable(current) = table.bind(cx).untag() else { unreachable!() };
        let (next, cost) = current.mapping_next(start, remaining)?;
        let Some((first, last, value)) = next else { return Ok(()) };
        anyhow::ensure!(cost <= remaining && remaining > 0, "map-char-table work budget exceeded");
        remaining -= cost.max(1);
        root!(value, cx);
        let key = if first == last {
            Object::from(first as i64)
        } else {
            let pair = range.bind(cx).cons().expect("rooted range cons");
            pair.set_car(Object::from(first as i64))?;
            pair.set_cdr(Object::from(last as i64))?;
            range.bind(cx)
        };
        root!(key, cx);
        if !matches!(
            function.bind(cx).untag(),
            ObjectType::NIL
                | ObjectType::Symbol(_)
                | ObjectType::Cons(_)
                | ObjectType::SubrFn(_)
                | ObjectType::ByteFn(_)
        ) {
            env.initialize_condition_metadata(cx);
            let data = list![function.bind(cx); cx];
            return Err(
                crate::eval::EvalError::signal(sym::INVALID_FUNCTION.into(), data, cx).into()
            );
        }
        let callable: Function = function.bind(cx).try_into().expect("function tag preflight");
        root!(callable, cx);
        call!(callable, key.bind(cx), value.bind(cx); env, cx)?;
        if last == 0x3fffff {
            return Ok(());
        }
        start = last + 1;
    }
}

#[cfg(test)]
mod mapping_tests {
    #[test]
    fn sparse_ranges_live_mutation_and_callbacks_match_gnu() {
        for source in [
            "(let ((m (make-char-table nil nil)) out) (aset m 65 'a) (aset m 66 'a) (aset m 67 'a) (aset m 90 'z) (list (map-char-table #'(lambda (k v) (setq out (cons (list (if (consp k) (cons (car k) (cdr k)) k) v) out))) m) (nreverse out)))",
            "(let ((m (make-char-table nil nil)) out) (aset m 65 'a) (aset m 90 'z) (map-char-table #'(lambda (k v) (setq out (cons (list k v) out)) (if (= k 65) (progn (aset m 70 'b) (aset m 90 'changed)))) m) (nreverse out))",
            "(let ((m (make-char-table nil nil)) (n 0)) (aset m 65 t) (aset m 90 t) (list (condition-case e (map-char-table #'(lambda (k v) (setq n (1+ n)) (signal 'error '(\"stop\"))) m) (error (car e))) n))",
            "(map-char-table 42 (make-char-table nil nil))",
            "(let ((m (make-char-table nil nil)) out) (aset m 65 m) (map-char-table #'(lambda (k v) (garbage-collect) (setq out (eq v m))) m) out)",
            "(let ((m (make-char-table nil nil)) out) (aset m 65 (list 'a)) (aset m 90 (list 'b)) (map-char-table #'(lambda (k v) (garbage-collect) (setq out (cons (list k (car v)) out))) m) (nreverse out))",
        ] {
            eprintln!("map-char-table oracle: {source}");
            crate::assert_elprop!("{}", source);
        }
    }
}

#[defun]
fn char_table_subtype(table: &CharTable) -> Object<'_> {
    table.purpose()
}
#[defun]
fn char_table_parent(table: &CharTable) -> Option<&CharTable> {
    table.parent()
}

#[cfg(test)]
mod purpose_tests {
    #[test]
    fn generic_zero_extra_purposes_preserve_metadata_and_values_like_gnu() {
        for source in [
            "(let ((table (make-char-table 'regexp-opt-charset))) (list (char-table-subtype table) (aref table 97)))",
            "(let* ((purpose (make-symbol \"rune-generic-table\")) (table (make-char-table purpose 'initial))) (garbage-collect) (list (eq purpose (char-table-subtype table)) (aref table 65)))",
            "(let* ((purpose (make-symbol \"rune-zero-table\"))) (put purpose 'char-table-extra-slots 0) (let ((table (make-char-table purpose 7))) (list (eq purpose (char-table-subtype table)) (aref table 66))))",
            "(let ((table (make-char-table 'regexp-opt-charset)) out) (aset table 65 t) (aset table 66 t) (map-char-table #'(lambda (key value) (setq out (cons (list (if (consp key) (cons (car key) (cdr key)) key) value) out))) table) out)",
        ] {crate::assert_elprop!("{}",source);}
    }
    #[test]
    fn unsupported_extra_slots_do_not_silently_drop_metadata() {
        crate::interpreter::assert_lisp("(let ((purpose (make-symbol \"rune-extra-table\"))) (put purpose 'char-table-extra-slots 1) (condition-case nil (make-char-table purpose) (error t)))","t");
    }
}
