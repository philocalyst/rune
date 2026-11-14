//! GNU30's representation-specific type query. This never walks object graphs.
use crate::core::{env::sym, object::{Object, ObjectType, NIL, TRUE}};
use anyhow::{Result, bail};
use rune_macros::defun;

defsym!(FIXNUM);
defsym!(BIGNUM);
defsym!(BOOLEAN);
defsym!(BYTE_CODE_FUNCTION);
defsym!(PRIMITIVE_FUNCTION);

#[defun]
fn cl_type_of(object: Object<'_>) -> Result<Object<'_>> {
    Ok(match object.untag() {
        ObjectType::Int(_) => sym::FIXNUM.into(),
        ObjectType::BigInt(_) => sym::BIGNUM.into(),
        ObjectType::Symbol(_) if object.ptr_eq(NIL) => sym::NULL.into(),
        ObjectType::Symbol(_) if object.ptr_eq(TRUE) => sym::BOOLEAN.into(),
        ObjectType::Symbol(_) => sym::SYMBOL.into(),
        ObjectType::Cons(_) => sym::CONS.into(),
        ObjectType::Float(_) => sym::FLOAT.into(),
        ObjectType::String(_) | ObjectType::ByteString(_) => sym::STRING.into(),
        ObjectType::Vec(_) => sym::VECTOR.into(),
        ObjectType::ByteFn(_) => sym::BYTE_CODE_FUNCTION.into(),
        ObjectType::SubrFn(_) => sym::PRIMITIVE_FUNCTION.into(),
        ObjectType::Record(record) => {
            let Some(tag) = record.first().map(|slot| slot.get()) else {
                bail!("cl-type-of cannot classify an empty internal record")
            };
            // GNU decodes a class descriptor only one level. The name field
            // can itself be any object; do not intern, validate, or recurse.
            match tag.untag() {
                ObjectType::Record(class) if class.len() > 1 => class[1].get(),
                _ => tag,
            }
        },
        ObjectType::HashTable(_) => sym::HASH_TABLE.into(),
        ObjectType::Buffer(_) => sym::BUFFER.into(),
        ObjectType::CharTable(_) => sym::CHAR_TABLE.into(),
        ObjectType::Process(_) => sym::PROCESS.into(),
        ObjectType::Frame(_) => sym::FRAME.into(),
        ObjectType::Overlay(_) => sym::OVERLAY.into(),
        ObjectType::Marker(_) => sym::MARKER.into(),
        ObjectType::Obarray(_) => sym::OBARRAY.into(),
        ObjectType::ChannelSender(_) => sym::CHANNEL_SENDER.into(),
        ObjectType::ChannelReceiver(_) => sym::CHANNEL_RECEIVER.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{env::intern, gc::{Context, RootSet}, object::RecordBuilder};
    #[test]
    fn cl_type_of_supported_gnu_representations_and_record_identity() {
        crate::assert_elprop!("{}", r#"(list (cl-type-of nil) (cl-type-of t) (cl-type-of :keyword) (cl-type-of (make-symbol "nil")) (cl-type-of (make-symbol "t")) (cl-type-of 7) (cl-type-of 1.5) (cl-type-of "abc") (cl-type-of (make-string 1 255)) (cl-type-of [1]) (cl-type-of '(lambda (x) x)) (cl-type-of '(closure nil nil 7)) (cl-type-of (symbol-function 'car)) (cl-type-of (make-byte-code 0 "\207" [] 0)))"#);
        crate::interpreter::assert_lisp(r#"(let ((x (vector nil))) (aset x 0 x) (let ((r (record x))) (list (eq (cl-type-of r) x) (progn (garbage-collect) (eq (cl-type-of r) x)))))"#, "(t t)");
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let tag = cx.add(intern("rune-cl-type-record", &cx));
        let mut slots = cx.vec_new(); slots.push(tag);
        let short = cx.add(RecordBuilder(slots));
        let mut outer = cx.vec_new(); outer.push(short);
        assert!(cl_type_of(cx.add(RecordBuilder(outer))).unwrap().ptr_eq(short));
        let mut slots = cx.vec_new(); slots.push(tag); slots.push(cx.add(7_i64));
        let class = cx.add(RecordBuilder(slots));
        let mut outer = cx.vec_new(); outer.push(class);
        assert!(cl_type_of(cx.add(RecordBuilder(outer))).unwrap().ptr_eq(cx.add(7_i64)));
        let empty = cx.add(RecordBuilder(cx.vec_new()));
        assert!(cl_type_of(empty).is_err());
    }
}
