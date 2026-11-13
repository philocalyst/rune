//! Character and string utilities.
use crate::core::{
    gc::Context,
    object::{Gc, Object, OptionalFlag, int_to_char},
};
use anyhow::{Result, anyhow, ensure};
use rune_macros::defun;

#[defun]
fn unibyte_string(bytes: &[Gc<i64>]) -> Result<Vec<u8>> {
    let unibyte: Result<Vec<u8>, _> = bytes.iter().map(|x| u8::try_from(x.untag())).collect();
    Ok(unibyte?)
}

#[defun]
fn max_char(unicode: OptionalFlag) -> usize {
    if unicode.is_some() { std::char::MAX as usize } else { 0x3F_FFFF }
}

#[defun]
fn characterp(obj: Object) -> bool {
    char::try_from(obj).is_ok()
}

#[defun]
fn string(characters: &[Gc<i64>]) -> Result<String> {
    let string: Result<_, _> = characters.iter().map(|x| int_to_char(x.untag())).collect();
    Ok(string?)
}

#[defun]
fn make_string<'ob>(
    length: usize,
    init: usize,
    multibyte: OptionalFlag,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    // GNU make-string selects unibyte storage only for ASCII unless the
    // caller explicitly requests multibyte. Non-ASCII scalars select it even
    // when MULTIBYTE is omitted (alloc.c:Fmake_string).
    if multibyte.is_some() || init >= 128 {
        let chr = int_to_char(i64::try_from(init)?)?;
        let byte_len = length
            .checked_mul(chr.len_utf8())
            .ok_or_else(|| anyhow!("make-string byte length overflow"))?;
        // This is Rust's addressable allocation bound, not a resource budget.
        ensure!(byte_len <= isize::MAX as usize, "make-string byte length overflow");
        let mut string = cx.string_with_capacity(byte_len);
        for _ in 0..length {
            string.push(chr);
        }
        Ok(cx.add(string))
    } else {
        let chr = u8::try_from(init)?;
        ensure!(length <= isize::MAX as usize, "make-string byte length overflow");
        let string: Vec<_> = (0..length).map(|_| chr).collect();
        Ok(cx.add(string))
    }
}

#[cfg(test)]
mod m0_make_string_regressions {
    use super::*;
    use crate::{
        core::{gc::RootSet, object::ObjectType},
        interpreter::assert_lisp,
    };

    #[test]
    fn non_ascii_init_automatically_selects_multibyte_storage() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for character in ['\u{80}', '\u{e9}', '\u{3bb}', '\u{1f600}'] {
            let result = make_string(2, character as usize, None, &cx).unwrap();
            assert!(matches!(result.untag(), ObjectType::String(_)));
            assert_eq!(result, cx.add(character.to_string().repeat(2)));
        }
        let ascii = make_string(2, 65, None, &cx).unwrap();
        assert!(matches!(ascii.untag(), ObjectType::ByteString(_)));
        assert_eq!(ascii, cx.add(vec![65_u8, 65]));
        let forced = make_string(2, 65, Some(()), &cx).unwrap();
        assert!(matches!(forced.untag(), ObjectType::String(_)));
        assert_eq!(forced, cx.add("AA"));
        drop(cx);
        assert_lisp("(make-string 2 955)", "\"\u{3bb}\u{3bb}\"");
        assert_lisp("(multibyte-string-p (make-string 2 233))", "t");
        assert_lisp("(multibyte-string-p (make-string 2 65))", "nil");
        assert_lisp("(multibyte-string-p (make-string 2 65 t))", "t");
    }

    #[test]
    fn impossible_string_sizes_error_before_allocation() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        // Multiplication overflow and non-overflowing but unaddressable sizes.
        for (length, init, forced) in [
            (usize::MAX, 233, None),
            (isize::MAX as usize, 233, None),
            (usize::MAX, 65, None),
            (usize::MAX, 65, Some(())),
        ] {
            assert!(make_string(length, init, forced, &cx).is_err());
        }
        assert!(make_string(0, 0x110000, None, &cx).is_err());
    }
}
