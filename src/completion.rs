//! Byte-preserving, callback-free list completion for genuine regexp-opt consumers.
use crate::core::{
    env::{Env, sym},
    gc::{Context, Rt},
    object::{NIL, Object, ObjectType, TRUE},
};
use anyhow::{Result, ensure};
use rune_macros::defun;
defvar!(COMPLETION_IGNORE_CASE);
defvar!(COMPLETION_REGEXP_LIST);
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 65536;
// Internal raw-byte units are disjoint from all Unicode scalar values. ASCII
// bytes share their ordinary scalar unit. Never decode byte strings as UTF8.
const RAW_UNIT: u32 = 0x11_0000;
#[derive(Clone, Copy)]
enum Storage<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}
#[derive(Clone, Copy)]
struct Name<'a> {
    storage: Storage<'a>,
    source: Option<Object<'a>>,
    length: usize,
}
enum Units<'a> {
    Text(std::str::Chars<'a>),
    Bytes(std::slice::Iter<'a, u8>),
}
impl Iterator for Units<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        match self {
            Self::Text(chars) => chars.next().map(u32::from),
            Self::Bytes(bytes) => bytes
                .next()
                .map(|b| if *b < 128 { u32::from(*b) } else { RAW_UNIT + u32::from(*b) }),
        }
    }
}
fn same(a: u32, b: u32, fold: bool) -> bool {
    let lower = |v| if (65..=90).contains(&v) { v + 32 } else { v };
    if fold { lower(a) == lower(b) } else { a == b }
}
impl<'a> Name<'a> {
    fn bytes(self) -> usize {
        match self.storage {
            Storage::Text(s) => s.len(),
            Storage::Bytes(s) => s.len(),
        }
    }
    fn case_supported(self) -> bool {
        match self.storage {
            Storage::Text(s) => s.is_ascii(),
            Storage::Bytes(_) => true,
        }
    }
    fn units(self) -> Units<'a> {
        match self.storage {
            Storage::Text(s) => Units::Text(s.chars()),
            Storage::Bytes(s) => Units::Bytes(s.iter()),
        }
    }
    fn starts(self, input: Self, fold: bool) -> bool {
        self.length >= input.length
            && self.units().zip(input.units()).all(|(a, b)| same(a, b, fold))
    }
    fn equal(self, other: Self) -> bool {
        self.length == other.length && self.starts(other, false)
    }
    fn common(self, other: Self, limit: usize, fold: bool) -> usize {
        self.units()
            .zip(other.units())
            .take(limit)
            .take_while(|(a, b)| same(*a, *b, fold))
            .count()
    }
    fn prefix(self, count: usize, cx: &'a Context) -> Result<Object<'a>> {
        if let Some(source) = self.source {
            validate_prefix_properties(source, count)?;
            return crate::fns::substring_lisp(source, Some(0), Some(count as i64), cx);
        }
        let Storage::Text(text) = self.storage else { unreachable!("symbol names are text") };
        Ok(cx.add(text.chars().take(count).collect::<String>()))
    }
}
// Public plist spines can be mutated. Validate only intersecting prefix runs
// before the existing preserving copier, so its list converter cannot format a
// malformed tail; bound total property work, not only each individual run.
fn validate_prefix_properties(source: Object<'_>, count: usize) -> Result<()> {
    let mut work = 0usize;
    for run in crate::core::object::string_property_runs(source)? {
        if run.start >= count {
            continue;
        }
        let mut tail = run.plist;
        let mut cells = 0usize;
        while !tail.is_nil() {
            ensure!(work < 65536, "Completion property copy exceeds work budget");
            work += 1;
            cells += 1;
            let ObjectType::Cons(pair) = tail.untag() else {
                anyhow::bail!("Malformed completion property plist");
            };
            tail = pair.cdr();
        }
        ensure!(cells % 2 == 0, "Odd completion property plist");
    }
    Ok(())
}
fn string_name(value: Object<'_>, input: bool) -> Result<Name<'_>> {
    let storage = match value.untag() {
        ObjectType::String(s) => Storage::Text(&**s),
        ObjectType::ByteString(s) => Storage::Bytes(&s),
        _ => unreachable!("string tag preflight"),
    };
    let bytes = match storage {
        Storage::Text(s) => s.len(),
        Storage::Bytes(s) => s.len(),
    };
    ensure!(
        bytes <= 1024 * 1024,
        if input {
            "Completion input exceeds 1 MiB limit"
        } else {
            "Completion string exceeds 1 MiB limit"
        }
    );
    let length = match storage {
        Storage::Text(s) => s.chars().count(),
        Storage::Bytes(s) => s.len(),
    };
    Ok(Name { storage, source: Some(value), length })
}
fn text(value: Object<'_>) -> Result<Option<Name<'_>>> {
    let value = match value.untag() {
        ObjectType::Cons(pair) => pair.car(),
        _ => value,
    };
    match value.untag() {
        ObjectType::String(_) | ObjectType::ByteString(_) => Ok(Some(string_name(value, false)?)),
        ObjectType::Symbol(symbol) => {
            let name = symbol.get().name();
            ensure!(name.len() <= 1024 * 1024, "Completion symbol exceeds 1 MiB limit");
            Ok(Some(Name {
                storage: Storage::Text(name),
                source: None,
                length: name.chars().count(),
            }))
        }
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
    if !matches!(string.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
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
    let input = string_name(string, true)?;
    ensure!(predicate.is_none_or(|v| v.is_nil()), "Completion predicates are unsupported");
    ensure!(
        crate::data::symbol_value(sym::COMPLETION_REGEXP_LIST, env, cx)?.is_nil(),
        "Completion regexp filters are unsupported"
    );
    let fold = !crate::data::symbol_value(sym::COMPLETION_IGNORE_CASE, env, cx)?.is_nil();
    ensure!(
        !fold || input.case_supported(),
        "Non-ASCII Unicode case-folded completion is unsupported"
    );
    ensure!(
        matches!(collection.untag(), ObjectType::Cons(_)) || collection.is_nil(),
        "Only list completion collections are supported"
    );
    let (mut tail, mut bytes, mut best, mut prefix_end, mut all_exact) =
        (collection, 0usize, None::<Name>, 0usize, true);
    let mut seen = std::collections::HashSet::new();
    while let ObjectType::Cons(pair) = tail.untag() {
        ensure!(seen.len() < MAX_ENTRIES, "Completion collection exceeds entry budget");
        seen.try_reserve(1)?;
        ensure!(
            seen.insert(tail.into_raw().addr()),
            "Cyclic completion collection is unsupported"
        );
        if let Some(candidate) = text(pair.car())? {
            bytes = bytes
                .checked_add(candidate.bytes())
                .ok_or_else(|| anyhow::anyhow!("Completion byte budget overflow"))?;
            ensure!(bytes <= MAX_BYTES, "Completion collection exceeds byte budget");
            ensure!(
                !fold || candidate.case_supported(),
                "Non-ASCII Unicode case-folded completion is unsupported"
            );
            if candidate.starts(input, fold) {
                all_exact &= candidate.equal(input);
                if let Some(previous) = best {
                    prefix_end = previous.common(candidate, prefix_end, fold);
                    if fold {
                        let candidate_full = prefix_end == candidate.length;
                        let previous_full = prefix_end == previous.length;
                        if (candidate_full && prefix_end < previous.length)
                            || (candidate_full == previous_full
                                && candidate.starts(input, false)
                                && !previous.starts(input, false))
                        {
                            best = Some(candidate);
                        }
                    }
                } else {
                    best = Some(candidate);
                    prefix_end = candidate.length;
                }
            }
        }
        tail = pair.cdr();
    }
    let Some(best) = best else { return Ok(NIL) };
    if all_exact {
        return Ok(TRUE);
    };
    best.prefix(prefix_end, cx)
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
        let raw = cx.add(vec![255_u8; 1024 * 1024 + 1]);
        assert_eq!(
            text(raw).err().expect("raw byte limit").to_string(),
            "Completion string exceeds 1 MiB limit"
        );
        assert_eq!(
            try_completion(raw, NIL, None, env, cx).unwrap_err().to_string(),
            "Completion input exceeds 1 MiB limit"
        );
        let large = cx.add("λ".repeat(600_000));
        assert_eq!(
            text(large).err().expect("byte limit").to_string(),
            "Completion string exceeds 1 MiB limit"
        );
        assert_eq!(
            try_completion(large, NIL, None, env, cx).unwrap_err().to_string(),
            "Completion input exceeds 1 MiB limit"
        );
    }
    #[test]
    fn raw_and_mixed_string_prefixes_match_gnu_without_utf8_decoding() {
        for form in [
            r#"(let ((v (try-completion "\377" '("\377\200a" "\377\200b")))) (list (multibyte-string-p v) (length v) (aref v 0) (aref v 1)))"#,
            r#"(let ((v (try-completion "" '("\377a" "\377b")))) (list (multibyte-string-p v) (length v) (aref v 0)))"#,
            r#"(let ((v (try-completion (make-string 1 97) '("aλx" "aλy")))) (list (multibyte-string-p v) (length v) (aref v 0) (aref v 1)))"#,
            r#"(try-completion "\377" '("ÿa" "ÿb"))"#,
            r#"(try-completion "ÿ" '("\377a" "\377b"))"#,
            r#"(try-completion "\303\251" '("éa" "éb"))"#,
            r#"(try-completion "é" '("\303\251a" "\303\251b"))"#,
            r#"(let ((v (try-completion "" '("\377a" "ÿb")))) (list (multibyte-string-p v) (length v)))"#,
            r#"(let ((v (try-completion "" '("ÿb" "\377a")))) (list (multibyte-string-p v) (length v)))"#,
            r#"(let ((a (concat (make-string 1 97 t) (make-string 1 98 t) (make-string 1 99 t))) (b (concat (make-string 1 97) (make-string 1 98) (make-string 1 100)))) (let ((v (try-completion "a" (list a b)))) (list (multibyte-string-p v) (length v) (aref v 1))))"#,
            r#"(let ((a (concat (make-string 1 97 t) (make-string 1 98 t) (make-string 1 99 t))) (b (concat (make-string 1 97) (make-string 1 98) (make-string 1 100)))) (let ((v (try-completion "a" (list b a)))) (list (multibyte-string-p v) (length v) (aref v 1))))"#,
        ] {
            crate::assert_elprop!(
                "(let ((completion-ignore-case nil) (completion-regexp-list nil)) {})",
                form
            );
        }
    }
    #[test]
    fn selected_candidate_prefix_keeps_storage_properties_and_shallow_values() {
        for form in [
            r#"(let* ((value (list 7)) (a (propertize "abc" 'p value)) (b (propertize "abd" 'p '(9))) (input (propertize "a" 'p 'input)) (v (try-completion input (list a b)))) (list (equal v "ab") (eq (multibyte-string-p a) (multibyte-string-p v)) (eq (get-text-property 0 'p v) value) (eq (text-properties-at 0 a) (text-properties-at 0 v))))"#,
            r#"(let* ((value (vector nil)) (a (propertize "\377\200a" 'p value)) (b (propertize "\377\200b" 'p '(9))) (v (try-completion "\377" (list a b)))) (aset value 0 value) (garbage-collect) (list (multibyte-string-p v) (length v) (aref v 0) (aref v 1) (eq (get-text-property 1 'p v) value)))"#,
        ] {
            crate::assert_elprop!(
                "(let ((completion-ignore-case nil) (completion-regexp-list nil)) {})",
                form
            );
        }
        for form in [
            r#"(try-completion "\311" '("\351a"))"#,
            r#"(let ((v (try-completion "\377" '("\377a" "\377b")))) (list (multibyte-string-p v) (length v) (aref v 0)))"#,
        ] {
            crate::assert_elprop!(
                "(let ((completion-ignore-case t) (completion-regexp-list nil)) {})",
                form
            );
        }
        crate::assert_elprop!(
            r#"(let* ((completion-ignore-case t) (completion-regexp-list nil) (a (concat (make-string 1 65 t) (make-string 1 66 t))) (b (concat (make-string 1 97) (make-string 1 66)))) (setq a (propertize a 'p 1) b (propertize b 'p 2)) (let ((v (try-completion "a" (list a b)))) (list (multibyte-string-p v) (aref v 0) (aref v 1) (get-text-property 0 'p v))))"#
        );
    }
    #[test]
    fn prefix_property_spines_are_independent_and_values_are_shallow() {
        use super::*;
        use crate::core::{cons::Cons, gc::RootSet, object::StringPropertyRun};
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let original_value = Cons::new1(7, cx);
        let plist = Cons::new(sym::TRUE, Cons::new1(original_value, cx), cx);
        let source = cx.add("abc");
        let ObjectType::String(value) = source.untag() else { unreachable!() };
        // SAFETY: source, plist spine and shallow value are all owned by cx.
        unsafe {
            value
                .set_property_runs(
                    &[StringPropertyRun { start: 0, end: 3, plist: plist.into() }],
                    cx,
                )
                .unwrap();
        }
        let names = Cons::new(source, Cons::new1(cx.add("abd"), cx), cx);
        let result = try_completion(cx.add("a"), names.into(), None, env, cx).unwrap();
        let copied = crate::core::object::string_property_runs(result).unwrap()[0].plist;
        assert!(!copied.ptr_eq(Object::from(plist)));
        let value_cell = copied.cons().unwrap().cdr().cons().unwrap();
        assert!(value_cell.car().ptr_eq(Object::from(original_value)));
        value_cell.set_car(42.into()).unwrap();
        assert!(plist.cdr().cons().unwrap().car().ptr_eq(Object::from(original_value)));
    }
    #[test]
    fn malformed_prefix_property_spines_reject_without_rendering() {
        use super::*;
        use crate::core::{cons::Cons, gc::RootSet, object::StringPropertyRun};
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let invalid = cx.add(vec![NIL]);
        let plist = Cons::new(sym::TRUE, invalid, cx);
        let source = cx.add("abc");
        let ObjectType::String(value) = source.untag() else { unreachable!() };
        // SAFETY: every edge belongs to cx. Deliberately malformed public-style
        // plist data is tested; no collection occurs during this setup/call.
        unsafe {
            value
                .set_property_runs(
                    &[StringPropertyRun { start: 0, end: 3, plist: plist.into() }],
                    cx,
                )
                .unwrap();
        }
        let names = Cons::new(source, Cons::new1(cx.add("abd"), cx), cx);
        assert_eq!(
            try_completion(cx.add("a"), names.into(), None, env, cx)
                .unwrap_err()
                .to_string(),
            "Malformed completion property plist"
        );
        plist.set_cdr(plist.into()).unwrap();
        assert_eq!(
            try_completion(cx.add("a"), names.into(), None, env, cx)
                .unwrap_err()
                .to_string(),
            "Completion property copy exceeds work budget"
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
            r#"(list (let ((completion-regexp-list '("a"))) (condition-case nil (try-completion "" '("abc")) (error 'unsupported))) (condition-case nil (try-completion "" '("abc") (lambda(x)t)) (error 'unsupported)))"#,
            "(unsupported unsupported)",
        );
    }
}
