//! Fail-closed barriers until readable printing/formatting preserve string runs.
use crate::core::object::{Object, ObjectType};
use anyhow::{Result, ensure};

enum Task<'a> { Enter(Object<'a>, usize), Leave(usize) }

/// Source must stay callback/GC-free. Callers hold INTERNED_SYMBOLS until their
/// subsequent pure Rust rendering finishes, excluding global hash insertion.
/// Other published global container mutations only remove edges. Local graph
/// mutation is excluded by the owner's shared Context/evaluation discipline.
pub(crate) fn reject_property_strings(object: Object<'_>) -> Result<()> {
    let mut tasks = Vec::new();
    tasks.try_reserve(1)?;
    tasks.push(Task::Enter(object, 0));
    let mut active = std::collections::HashSet::new();
    let mut work = 0usize;
    while let Some(task) = tasks.pop() {
        let (object, depth) = match task {
            Task::Leave(id) => { active.remove(&id); continue; }
            Task::Enter(object, depth) => (object, depth),
        };
        work += 1;
        ensure!(work <= 100_000 && depth <= 256, "Property-aware serialization traversal limit exceeded");
        let mut children = Vec::new();
        match object.untag() {
            ObjectType::String(text) => ensure!(!text.has_properties(), "Formatting/serialization of property-bearing strings is unsupported"),
            ObjectType::ByteString(bytes) => ensure!(!bytes.has_properties(), "Formatting/serialization of property-bearing strings is unsupported"),
            ObjectType::Cons(pair) => { children.try_reserve_exact(2)?; children.extend([pair.car(), pair.cdr()]); }
            ObjectType::Vec(vector) => {
                ensure!(vector.len() <= 100_000 - work, "Property-aware serialization edge limit exceeded");
                children.try_reserve_exact(vector.len())?;
                children.extend(vector.iter().map(|cell| cell.get()));
            }
            ObjectType::Record(record) => {
                ensure!(record.len() <= 100_000 - work, "Property-aware serialization edge limit exceeded");
                children.try_reserve_exact(record.len())?;
                children.extend(record.iter().map(|cell| cell.get()));
            }
            ObjectType::ByteFn(function) => {
                ensure!(function.consts().len() <= 100_000 - work, "Property-aware serialization edge limit exceeded");
                children.try_reserve_exact(function.consts().len())?;
                children.extend_from_slice(function.consts());
            }
            ObjectType::HashTable(table) => children = table.clone_children_snapshot(100_000 - work)?,
            ObjectType::CharTable(table) => children = table.clone_children_snapshot(100_000 - work)?,
            _ => {}, // Names/numbers and opaque native handles print no Lisp children.
        }
        if children.is_empty() { continue; }
        let id = object.into_raw().addr();
        active.try_reserve(1)?;
        ensure!(active.insert(id), "Cyclic graph serialization is unsupported");
        ensure!(tasks.len().checked_add(children.len()).and_then(|n| n.checked_add(1)).is_some_and(|n| n <= 100_000), "Property-aware serialization task limit exceeded");
        tasks.try_reserve(children.len() + 1)?;
        tasks.push(Task::Leave(id));
        for child in children.into_iter().rev() { tasks.push(Task::Enter(child, depth + 1)); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::interpreter::assert_lisp;
    #[test]
    fn formatting_and_printing_reject_properties_in_controls_and_nested_values() {
        for source in [
            r#"(format "%s" (propertize "x" 'p 1))"#,
            r#"(format "%S" (list (propertize "x" 'p 1)))"#,
            r#"(format "%s" (vector (propertize "x" 'p 1)))"#,
            r#"(format (propertize "%s" 'p 1) "x")"#,
            r#"(format-message (propertize "%s" 'p 1) "x")"#,
            r#"(message "%s" (propertize "x" 'p 1))"#,
            r#"(prin1-to-string (propertize "x" 'p 1))"#,
            r#"(prin1-to-string (list (vector (propertize "\377x" 'p 1))))"#,
        ] {
            assert_lisp(&format!("(condition-case nil {source} (error 'blocked))"), "blocked");
        }
        // A failed barrier releases the global gate; unused arguments are not
        // rendered and need not lose or transfer their properties.
        assert_lisp(r#"(progn (condition-case nil (prin1-to-string (propertize "x" 'p 1)) (error nil)) (list (format "%s" 7) (format "plain" (propertize "x" 'p 1))))"#, r#"("7" "plain")"#);
    }
    #[test]
    fn unsupported_case_and_replacement_transforms_reject_before_losing_properties() {
        for name in ["capitalize", "upcase", "downcase", "upcase-initials"] {
            assert_lisp(&format!(r#"(condition-case nil ({name} (propertize "abc" 'p 1)) (error 'blocked))"#), "blocked");
        }
        assert_lisp(r#"(let ((s (propertize "abc" 'p 1))) (set-match-data '(0 1))
            (list (condition-case nil (replace-match "x" nil nil s) (error 'blocked))
                  (condition-case nil (replace-match (propertize "x" 'p 2) nil nil "abc") (error 'blocked))
                  (equal s "abc") (get-text-property 0 'p s) (match-beginning 0) (match-end 0)))"#,
            "(blocked blocked t 1 0 1)");
    }
    #[test]
    fn intentional_property_stripping_and_supported_reuse_match_gnu() {
        crate::assert_elprop!("{}", r#"(let ((s (propertize "abc" 'p 1)))
            (list (get-text-property 0 'p (regexp-quote s))
                  (get-text-property 0 'p (regexp-quote (propertize "a.b" 'p 1)))
                  (get-text-property 0 'p (decode-coding-string s 'utf-8))
                  (get-text-property 0 'p (encode-coding-string s 'utf-8))
                  (get-text-property 0 'p (base64-encode-string s))
                  (let ((v (decode-coding-string s 'utf-8 t)))
                    (list (eq s v) (get-text-property 0 'p v)))))"#);
    }

    #[test]
    fn serialization_rejects_cycles_and_allows_repeated_plain_dags() {
        assert_lisp(r#"(let ((x (cons 1 nil))) (setcdr x x) (condition-case nil (prin1-to-string x) (error 'bounded)))"#, "bounded");
        assert_lisp(r#"(let ((x (list "plain"))) (list (stringp (prin1-to-string (list x x))) (format "%s" (list x x))))"#, r#"(t "((plain) (plain))")"#);
    }
}
