//! Callback-free list completion for genuine regexp-opt/startup consumers.
use crate::core::{
    env::{Env, sym},
    gc::{Context, Rt},
    object::{NIL, Object, ObjectType, TRUE},
};
use anyhow::{Result, bail, ensure};
use rune_macros::defun;
defvar!(COMPLETION_IGNORE_CASE);
defvar!(COMPLETION_REGEXP_LIST);
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 65536;
fn text(value: Object<'_>) -> Result<Option<&str>> {
    let value = match value.untag() {
        ObjectType::Cons(pair) => pair.car(),
        _ => value,
    };
    match value.untag() {
        ObjectType::String(value) => {
            ensure!(value.as_bytes().len() <= 1024 * 1024, "Completion string exceeds 1 MiB limit");
            ensure!(!value.has_properties(), "Property-bearing completion strings are unsupported");
            Ok(Some(&**value))
        }
        ObjectType::Symbol(symbol) => {
            let name = symbol.get().name();
            ensure!(name.len() <= 1024 * 1024, "Completion symbol exceeds 1 MiB limit");
            Ok(Some(name))
        }
        ObjectType::ByteString(_) => bail!("Raw-byte completion strings are unsupported"),
        _ => Ok(None),
    }
}
#[defun]
fn try_completion<'a>(
    string: Object<'a>,
    collection: Object<'a>,
    predicate: Option<Object>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let input = match string.untag() {
        ObjectType::String(input) => input,
        ObjectType::ByteString(_) => bail!("Raw-byte completion input is unsupported"),
        _ => {
            env.initialize_condition_metadata(cx);
            let data = crate::core::cons::Cons::new(
                sym::STRINGP,
                crate::core::cons::Cons::new1(string, cx),
                cx,
            )
            .into();
            return Err(
                crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into()
            );
        }
    };
    ensure!(input.as_bytes().len() <= 1024 * 1024, "Completion input exceeds 1 MiB limit");
    ensure!(!input.has_properties(), "Property-bearing completion input is unsupported");
    ensure!(
        predicate.is_none_or(|value| value.is_nil()),
        "Completion predicates are unsupported"
    );
    ensure!(
        crate::data::symbol_value(sym::COMPLETION_REGEXP_LIST, env, cx)?.is_nil(),
        "Completion regexp filters are unsupported"
    );
    let fold = !crate::data::symbol_value(sym::COMPLETION_IGNORE_CASE, env, cx)?.is_nil();
    ensure!(!fold || input.is_ascii(), "Non-ASCII case-folded completion is unsupported");
    ensure!(
        matches!(collection.untag(), ObjectType::Cons(_)) || collection.is_nil(),
        "Only list completion collections are supported"
    );
    let mut tail = collection;
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0usize;
    let mut best: Option<&str> = None;
    let mut prefix_end = 0usize;
    let mut all_exact = true;
    while let ObjectType::Cons(pair) = tail.untag() {
        ensure!(seen.len() < MAX_ENTRIES, "Completion collection exceeds entry budget");
        seen.try_reserve(1)?;
        ensure!(
            seen.insert(tail.into_raw().addr()),
            "Cyclic completion collection is unsupported"
        );
        if let Some(candidate) = text(pair.car())? {
            bytes = bytes
                .checked_add(candidate.len())
                .ok_or_else(|| anyhow::anyhow!("Completion byte budget overflow"))?;
            ensure!(bytes <= MAX_BYTES, "Completion collection exceeds byte budget");
            ensure!(
                !fold || candidate.is_ascii(),
                "Non-ASCII case-folded completion is unsupported"
            );
            let matched = if fold {
                candidate
                    .as_bytes()
                    .get(..input.as_bytes().len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(input.as_bytes()))
            } else {
                candidate.starts_with(&**input)
            };
            if matched {
                all_exact &= candidate == &**input;
                if let Some(previous) = best {
                    let old_prefix = &previous[..prefix_end];
                    let mut end = 0;
                    for ((offset, a), b) in old_prefix.char_indices().zip(candidate.chars()) {
                        if if fold { a.eq_ignore_ascii_case(&b) } else { a == b } {
                            end = offset + a.len_utf8();
                        } else {
                            break;
                        }
                    }
                    prefix_end = end;
                    if fold {
                        let candidate_full = prefix_end == candidate.len();
                        let previous_full = prefix_end == previous.len();
                        if (candidate_full && prefix_end < previous.len())
                            || (candidate_full == previous_full
                                && candidate.starts_with(&**input)
                                && !previous.starts_with(&**input))
                        {
                            best = Some(candidate);
                        }
                    }
                } else {
                    best = Some(candidate);
                    prefix_end = candidate.len();
                }
            }
        }
        tail = pair.cdr();
    }
    let Some(best) = best else { return Ok(NIL) };
    if all_exact {
        return Ok(TRUE);
    };
    Ok(cx.add(best[..prefix_end].to_owned()))
}
#[cfg(test)]
mod tests {
    #[test]
    fn literal_list_completion_matches_gnu() {
        for form in [
            r#"(try-completion "" nil)"#,
            r#"(try-completion "z" '("abc" "abd"))"#,
            r#"(try-completion "a" '("abc" "abd"))"#,
            r#"(try-completion "abc" '("abc"))"#,
            r#"(try-completion "abc" '("abc" "abc"))"#,
            r#"(try-completion "abc" '("abc" "abcd"))"#,
            r#"(try-completion "" '(""))"#,
            r#"(try-completion "λ" '("λ🙂a" "λ🙂b"))"#,
            r#"(try-completion "a" '(("abc" . 1) ("abd" . 2)))"#,
            r#"(try-completion "rune-" '(rune-abc rune-abd))"#,
            r#"(try-completion "n" '(nil))"#,
            r#"(try-completion "nil" '(nil))"#,
            r#"(try-completion "n" '((nil . 7)))"#,
            r#"(try-completion "" '(nil "abc"))"#,
            r#"(try-completion "a" '(42 "abc" . ignored))"#,
            r#"(progn (set-match-data '(7 8 9 10)) (let ((old (match-data))) (try-completion "a" '("abc" "abd")) (equal old (match-data))))"#,
        ] {
            crate::assert_elprop!(
                "(let ((completion-ignore-case nil) (completion-regexp-list nil)) {})",
                form
            );
        }
        for form in [
            r#"(try-completion "a" '("AbC" "abcD"))"#,
            r#"(try-completion "abc" '("AbC" "abc"))"#,
            r#"(try-completion "abc" '("AbC" "abcd"))"#,
            r#"(try-completion "abc" '("AbC" "AbC"))"#,
            r#"(try-completion "a" '("AB" "aB"))"#,
            r#"(try-completion "A" '("ab" "AB"))"#,
            r#"(try-completion "ab" '("ABCD" "abc"))"#,
            r#"(try-completion "ABC" '("abc" "AbC"))"#,
        ] {
            crate::assert_elprop!(
                "(let ((completion-ignore-case t) (completion-regexp-list nil)) {})",
                form
            );
        }
    }
    #[test]
    fn uninterned_names_and_result_survive_moving_collection_without_match_data_replacement() {
        use super::*;
        use crate::core::{cons::Cons, gc::RootSet, object::Symbol};
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let a = Symbol::new_uninterned("λ🙂a", cx);
        let b = Symbol::new_uninterned("λ🙂b", cx);
        let names = Cons::new(a, Cons::new1(b, cx), cx);
        root!(names, Object::from(names), cx);
        env.match_data.set(crate::fns::slice_into_list(&[7.into(), 8.into()], None, cx));
        let previous = env.match_data.bind(cx);
        let result = try_completion(cx.add("λ"), names.bind(cx), None, env, cx).unwrap();
        assert!(env.match_data.bind(cx).ptr_eq(previous));
        root!(result, cx);
        cx.garbage_collect(true);
        assert_eq!(result.bind(cx), cx.add("λ🙂"));
        let result2 = try_completion(cx.add("λ"), names.bind(cx), None, env, cx).unwrap();
        assert_eq!(result2, cx.add("λ🙂"));
        assert_eq!(env.match_data.bind(cx).to_string(), "(7 8)");
    }
    #[test]
    fn unicode_byte_limits_precede_scalar_scanning_and_matching() {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let large = cx.add("λ".repeat(600_000));
        assert_eq!(text(large).unwrap_err().to_string(), "Completion string exceeds 1 MiB limit");
        assert_eq!(
            try_completion(large, NIL, None, env, cx).unwrap_err().to_string(),
            "Completion input exceeds 1 MiB limit"
        );
    }
    #[test]
    fn malformed_and_unsupported_graphs_fail_without_render_or_silent_property_loss() {
        crate::interpreter::assert_lisp(
            r#"(let ((v (vector nil))) (aset v 0 v) (condition-case e (try-completion v nil) (wrong-type-argument (list (car e) (car (cdr e)) (eq v (car (cdr (cdr e))))))))"#,
            "(wrong-type-argument stringp t)",
        );
        crate::interpreter::assert_lisp(
            r#"(let ((values (list "abc"))) (setcdr values values) (condition-case nil (try-completion "" values) (error 'bounded)))"#,
            "bounded",
        );
        crate::interpreter::assert_lisp(
            r#"(list (condition-case nil (try-completion "" (list (propertize "abc" 'p 7))) (error 'unsupported)) (let ((completion-regexp-list '("a"))) (condition-case nil (try-completion "" '("abc")) (error 'unsupported))) (condition-case nil (try-completion "" '("abc") (lambda(x)t)) (error 'unsupported)))"#,
            "(unsupported unsupported unsupported)",
        );
    }
}
