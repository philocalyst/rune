//! Character and string utilities.
use crate::core::{
    gc::Context,
    object::{Gc, Object, ObjectType, OptionalFlag, int_to_char},
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
fn characterp(obj: Object, _ignore: Option<Object>) -> bool {
    // GNU character codes include surrogates and values beyond Unicode, even
    // though the current UTF-8 string constructors only support Rust scalars.
    matches!(obj.untag(), ObjectType::Int(code) if (0..=0x3f_ffff).contains(&code))
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

#[cfg(test)]
mod m0_character_predicate_regressions {
    use super::*;
    use crate::{
        core::{gc::RootSet, object::NIL},
        interpreter::assert_lisp,
    };

    #[test]
    fn characterp_uses_gnu_character_code_range_not_unicode_scalars() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for code in [0_i64, 127, 0xd800, 0xdfff, 0x10ffff, 0x110000, 0x3fffff] {
            assert!(characterp(cx.add(code), None), "code {code}");
        }
        for code in [-1_i64, 0x400000, 1_i64 << 26] {
            assert!(!characterp(cx.add(code), None), "code {code}");
        }
        for noncharacter in
            [NIL, cx.add(65.0_f64), cx.add("a"), cx.add(num_bigint::BigInt::from(i64::MAX))]
        {
            assert!(!characterp(noncharacter, None));
        }
        drop(cx);
        assert_lisp("(characterp (max-char))", "t");
        assert_lisp("(characterp 55296)", "t");
        assert_lisp("(characterp 1114112)", "t");
        assert_lisp("(characterp 4194304)", "nil");
        // The historical optional second argument is accepted but ignored.
        assert_lisp("(characterp 1114112 t)", "t");
    }
}
