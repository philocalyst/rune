//! GNU's native initial syntax table, before characters.el customization.
use crate::core::{
    cons::Cons,
    env::sym,
    gc::Context,
    object::{CharTableInner, NIL, Object, ObjectType},
};
use anyhow::Result;
use rune_macros::defun;
defsym!(SYNTAX_TABLE);
defsym!(CHAR_TABLE_EXTRA_SLOTS);

pub(crate) fn create_standard_syntax_table<'a>(cx: &'a Context) -> Result<Object<'a>> {
    // Shared owner borrow prevents moving GC throughout construction. Descriptor
    // graph is owner-local and publication is performed by Context only after
    // the complete table is ready.
    let descriptors: Vec<Object<'_>> =
        (0..10).map(|code| Cons::new(code, NIL, cx).into()).collect();
    let inner = CharTableInner::new(Some(descriptors[0]))
        .with_purpose(sym::SYNTAX_TABLE.into())
        .with_seed(128, 0x3fffff, descriptors[2])?;
    let table_object = cx.add(inner);
    let ObjectType::CharTable(table) = table_object.untag() else { unreachable!() };
    for code in 0..128 {
        let syntax = match code {
            9 | 10 | 12 | 13 | 32 => 0,
            48..=57 | 65..=90 | 97..=122 | 36 | 37 => 2,
            95 | 45 | 43 | 42 | 47 | 38 | 124 | 60 | 62 | 61 => 3,
            34 => 7,
            92 => 9,
            _ => 1,
        };
        table.set(code, descriptors[syntax])?;
    }
    for (open, close) in [(40, 41), (91, 93), (123, 125)] {
        table.set(open, Cons::new(4, close as i64, cx).into())?;
        table.set(close, Cons::new(5, open as i64, cx).into())?;
    }
    Ok(table_object)
}
#[defun]
fn standard_syntax_table<'a>(cx: &'a Context) -> Result<Object<'a>> {
    cx.standard_syntax_table()
}
#[defun]
fn syntax_table_p(value: Object<'_>) -> bool {
    matches!(value.untag(), ObjectType::CharTable(table) if table.purpose().ptr_eq(Object::from(sym::SYNTAX_TABLE)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{gc::RootSet, object::IntoObject};
    use rune_core::macros::root;
    #[test]
    fn syntax_extra_slots_native_property_is_zero() {
        crate::interpreter::assert_lisp("(get 'syntax-table 'char-table-extra-slots)", "0");
        crate::interpreter::assert_lisp("(progn (put 'syntax-table 'char-table-extra-slots 7) (standard-syntax-table) (standard-syntax-table) (get 'syntax-table 'char-table-extra-slots))", "7");
    }
    #[test]
    fn declared_extra_slots_are_rejected_not_dropped() {
        crate::interpreter::assert_lisp("(progn (put 'syntax-table 'char-table-extra-slots 1) (condition-case nil (progn (make-char-table 'syntax-table) nil) (error t)))", "t");
    }
    #[test]
    fn native_standard_syntax_is_canonical_rooted_and_real() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(table, cx.standard_syntax_table().unwrap(), cx);
        for _ in 0..3 {
            cx.garbage_collect(true);
            let again = cx.standard_syntax_table().unwrap();
            assert!(again.ptr_eq(table.bind(cx)));
            assert!(syntax_table_p(again));
            let ObjectType::CharTable(table) = again.untag() else { unreachable!() };
            for (code, expected) in [
                (0, "(1)"),
                (9, "(0)"),
                (11, "(1)"),
                (32, "(0)"),
                (36, "(2)"),
                (65, "(2)"),
                (95, "(3)"),
                (34, "(7)"),
                (92, "(9)"),
                (127, "(1)"),
                (128, "(2)"),
                (955, "(2)"),
                (0x3fffff, "(2)"),
            ] {
                assert_eq!(table.get(code).unwrap().to_string(), expected, "code {code}");
            }
            assert_eq!(table.get(40).unwrap().to_string(), "(4 . 41)");
            assert_eq!(table.get(41).unwrap().to_string(), "(5 . 40)");
            assert!(table.mapping_next(0, 1000).is_err());
        }
    }
    #[test]
    fn inherited_syntax_and_cycle_defense_are_real() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let standard = cx.standard_syntax_table().unwrap();
        let ObjectType::CharTable(parent) = standard.untag() else { unreachable!() };
        let child = CharTableInner::new(None)
            .with_purpose(sym::SYNTAX_TABLE.into())
            .into_obj(&cx.block);
        child.untag().validate_parent(Some(parent)).unwrap();
        child.untag().set_parent(Some(parent)).unwrap();
        assert_eq!(child.untag().get(65).unwrap().to_string(), "(2)");
        child.untag().set(65, NIL).unwrap();
        assert_eq!(child.untag().get(65).unwrap().to_string(), "(2)");
        assert!(parent.validate_parent(Some(child.untag())).is_err());
        let plain = CharTableInner::new(None).into_obj(&cx.block);
        plain.untag().validate_parent(Some(parent)).unwrap();
        // Raw core setter remains available for intrinsic-copy cycle defense
        // fixtures. Public setter checks first; lookup independently fails closed.
        plain.untag().set_parent(Some(plain.untag())).unwrap();
        assert!(plain.untag().get(65).is_err());
    }
}
