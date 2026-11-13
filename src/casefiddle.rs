//! String and character case conversion.
use anyhow::Result;
use std::ops::Range;

use crate::core::{gc::Rt, object::Object};
use crate::fns::StringOrChar;
use crate::{Context, Env};
use rune_macros::defun;
use text_buffer::Buffer as TextBuffer;

#[defun]
fn capitalize<'ob>(string_or_char: StringOrChar<'ob>, cx: &'ob Context<'ob>) -> Object<'ob> {
    match string_or_char {
        StringOrChar::String(s) => cx.add(casify_string(s, CaseMode::Capitalize)),
        StringOrChar::Char(c) => cx.add(casify_char(c, char::to_uppercase)),
    }
}

#[defun]
fn upcase<'ob>(string_or_char: StringOrChar<'ob>, cx: &'ob Context<'ob>) -> Object<'ob> {
    match string_or_char {
        StringOrChar::String(s) => cx.add(casify_string(s, CaseMode::Upcase)),
        StringOrChar::Char(c) => cx.add(casify_char(c, char::to_uppercase)),
    }
}

#[defun]
fn downcase<'ob>(string_or_char: StringOrChar<'ob>, cx: &'ob Context<'ob>) -> Object<'ob> {
    match string_or_char {
        StringOrChar::String(s) => cx.add(casify_string(s, CaseMode::Downcase)),
        StringOrChar::Char(c) => cx.add(casify_char(c, char::to_lowercase)),
    }
}

#[defun]
fn upcase_initials<'ob>(string_or_char: StringOrChar<'ob>, cx: &'ob Context<'ob>) -> Object<'ob> {
    match string_or_char {
        StringOrChar::String(s) => cx.add(casify_string(s, CaseMode::UpcaseInitials)),
        StringOrChar::Char(c) => cx.add(casify_char(c, char::to_uppercase)),
    }
}

#[defun]
fn upcase_word(offset: i64, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    casify_word(offset, env, cx, CaseMode::Upcase)
}

#[defun]
fn downcase_word(offset: i64, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    casify_word(offset, env, cx, CaseMode::Downcase)
}

#[defun]
fn capitalize_word(offset: i64, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    casify_word(offset, env, cx, CaseMode::Capitalize)
}

fn casify_word(offset: i64, env: &mut Rt<Env>, cx: &Context, mode: CaseMode) -> Result<()> {
    let buffer = env.current_buffer.get_mut();
    let text = &buffer.text;
    let range = if offset >= 0 { find_forward_word(text) } else { find_backward_word(text) };
    let (start, end) = (range.start, range.end);
    let (a, b) = text.slice(range);
    let replacement = casify_string(&(a.to_owned() + b), mode);
    buffer.replace_text(start, end, &replacement, cx)
}

fn casify_string(s: &str, mode: CaseMode) -> String {
    let mut out = String::with_capacity(s.len());

    for word in s.split_inclusive(|c: char| precedes_capitalization(c)) {
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            match mode {
                CaseMode::Downcase => out.extend(c.to_lowercase()),
                CaseMode::Upcase | CaseMode::Capitalize | CaseMode::UpcaseInitials => {
                    out.extend(c.to_uppercase())
                }
            }
        }
        for c in chars {
            match mode {
                CaseMode::Upcase => out.extend(c.to_uppercase()),
                CaseMode::Downcase | CaseMode::Capitalize => out.extend(c.to_lowercase()),
                CaseMode::UpcaseInitials => out.push(c),
            }
        }
    }
    out
}

fn precedes_capitalization(c: char) -> bool {
    !c.is_alphanumeric()
}

enum CaseMode {
    Downcase,
    Upcase,
    Capitalize,
    UpcaseInitials,
}

fn casify_char<T>(c: u64, f: impl Fn(char) -> T) -> u64
where
    T: Iterator<Item = char>,
{
    // emacs uses an identity function for invalid codepoints
    if c > crate::lisp::CHAR_MODIFIER_MASK {
        return c;
    }
    let Ok(u) = u32::try_from(c) else { return c };
    let Ok(chr) = char::try_from(u) else { return c };
    let mut cased = f(chr);
    let first = cased.next().unwrap();
    // if the char changes case to multiple characters, don't change case
    match cased.next() {
        Some(_) => c,
        None => first as u64,
    }
}

fn find_forward_word(buf: &TextBuffer) -> Range<usize> {
    let cursor = buf.cursor().chars();
    let (s1, s2) = buf.slice(cursor..);
    let end = cursor
        + s1.chars()
            .chain(s2.chars())
            .enumerate()
            .skip_while(|(_, c)| !c.is_alphanumeric())
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| buf.len_chars() - cursor);
    cursor..end
}

fn find_backward_word(buf: &TextBuffer) -> Range<usize> {
    let cursor = buf.cursor().chars();
    let (s1, s2) = buf.slice(..cursor);
    let start = cursor
        - s1.chars()
            .chain(s2.chars())
            .rev()
            .enumerate()
            .skip_while(|(_, c)| !c.is_alphanumeric())
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(0);
    start..cursor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootSet;

    #[test]
    fn test_downcase() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_eq!(downcase("The cat in the hat".into(), cx), "the cat in the hat");
        assert_eq!(downcase('x'.into(), cx), 'x');
        assert_eq!(downcase('X'.into(), cx), 'x');
    }

    #[test]
    fn test_upcase() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        // Emacs Doc Tests
        assert_eq!(upcase("The cat in the hat".into(), cx), "THE CAT IN THE HAT");
        assert_eq!(upcase("ﬁ".into(), cx), "FI");
        assert_eq!(upcase('ﬁ'.into(), cx), 'ﬁ');
        assert_eq!(upcase('x'.into(), cx), 'X');
        assert_eq!(upcase('X'.into(), cx), 'X');

        // Basic escape characters
        assert_eq!(upcase("\n".into(), cx), "\n");
        assert_eq!(upcase("\t".into(), cx), "\t");
        assert_eq!(upcase("\r".into(), cx), "\r");

        // Control characters
        assert_eq!(upcase("\u{0}".into(), cx), "\u{0}");
        assert_eq!(upcase("\u{1B}".into(), cx), "\u{1B}");
        assert_eq!(upcase("\u{7F}".into(), cx), "\u{7F}");

        // Non-ASCII characters
        assert_eq!(upcase("αβγ".into(), cx), "ΑΒΓ");
        assert_eq!(upcase("åäö".into(), cx), "ÅÄÖ");

        // Mixed content
        assert_eq!(upcase("hello\nworld".into(), cx), "HELLO\nWORLD");
        assert_eq!(upcase("foo\tbar".into(), cx), "FOO\tBAR");
        assert_eq!(upcase("path\\to\\file\"name\"".into(), cx), "PATH\\TO\\FILE\"NAME\"");

        // Invalid code points
        assert_eq!(upcase(StringOrChar::Char(0xD800), cx), 0xD800);
        assert_eq!(upcase(StringOrChar::Char(u64::MAX), cx), cx.add(u64::MAX));
    }

    #[test]
    fn test_capitalize() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);

        // Emacs doc tests
        assert_eq!(capitalize("The cat in the hat".into(), cx), "The Cat In The Hat");
        assert_eq!(capitalize("THE 77TH-HATTED CAT".into(), cx), "The 77th-Hatted Cat");
        assert_eq!(capitalize('x'.into(), cx), 'X');
        assert_eq!(capitalize('X'.into(), cx), 'X');
        assert_eq!(capitalize('ß'.into(), cx), 'ß');
        assert_eq!(capitalize("ß".into(), cx), "SS");

        // from elprop
        // TODO: implement syntax tables so it's known whether a character makes a word or symbol
        // // U+1D100 MUSICAL SYMBOL SINGLE BARLINE (Other-Symbol)
        // // U+0041 LATIN CAPITAL LETTER A
        // assert_eq!(capitalize("𝄀A", cx), Ok("𝄀a"));
        // // U+0024 DOLLAR SIGN (Currency-Symbol)
        // // U+0041 LATIN CAPITAL LETTER A
        // assert_eq!(capitalize("$A", cx), Ok("$a"));
        // // U+002D HYPHEN-MINUS (Dash-Punctuation)
        // // U+0041 LATIN CAPITAL LETTER A
        // assert_eq!(capitalize("-A", cx), Ok("-A"));
        // // U+005E CIRCUMFLEX ACCENT (Modifier-Symbol)
        // // U+0041 LATIN CAPITAL LETTER A
        // assert_eq!(capitalize("^A", cx), Ok("^A"));
        // // U+0FBE TIBETAN KU RU KHA (Other-Symbol)
        // // U+0041 LATIN CAPITAL LETTER A
        // assert_eq!(capitalize("྾A", cx), Ok("྾A"));
        // // U+10A50 KHAROSHTHI PUNCTUATION DOT (Other-Punctuation)
        // // U+104B0 OSAGE CAPITAL LETTER A
        // // (becomes) U+104D8 OSAGE SMALL LETTER A
        // assert_eq!(capitalize("𐩐𐒰", cx), Ok("𐩐𐓘"));
    }

    #[test]
    fn test_upcase_initials() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);

        // Emacs Doc Tests
        assert_eq!(upcase_initials("The CAT in the hAt".into(), cx), "The CAT In The HAt");
        assert_eq!(upcase_initials('x'.into(), cx), 'X');
        assert_eq!(upcase_initials('X'.into(), cx), 'X');
    }

    #[cfg(not(miri))] // Uses SIMD
    mod upcase_word {
        use crate::core::gc::Context;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;

        use super::*;

        #[test]
        fn expanding_case_updates_following_properties() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, new(Env), cx);
            let buffer = env.current_buffer.get_mut();
            buffer.insert(cx.add("ß tail"), cx).unwrap();
            buffer.textprops.lock(cx).unwrap().set_properties(3, 7, cx.add(42));
            buffer.text.set_cursor(0);
            upcase_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "SS tail");
            let properties = env.current_buffer.get().textprops.lock(cx).unwrap();
            assert!(properties.find(3).is_none());
            assert!(properties.find(4).is_some());
        }

        #[test]
        fn forward() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, new(Env), cx);

            // αβγ word
            // ^-----
            env.current_buffer.get_mut().text.insert("αβγ word");
            env.current_buffer.get_mut().text.set_cursor(0);
            upcase_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "ΑΒΓ word");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("ΑΒΓ woRd");
            env.current_buffer.get_mut().text.set_cursor(0);
            downcase_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "αβγ woRd");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("αΒΓ wORD");
            env.current_buffer.get_mut().text.set_cursor(0);
            capitalize_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "Αβγ wORD");
        }

        #[test]
        fn backward() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, new(Env), cx);

            // upcase αβγword
            //        -------^
            env.current_buffer.get_mut().text.insert("upcase αβγword ");
            env.current_buffer.get_mut().text.set_cursor(15);
            upcase_word(-1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "upcase ΑΒΓWORD ");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("dOwNcAsE αΒΓWord ");
            env.current_buffer.get_mut().text.set_cursor(17);
            downcase_word(-1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "dOwNcAsE αβγword ");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("cAPITALIZE αΒΓWORD ");
            env.current_buffer.get_mut().text.set_cursor(19);
            capitalize_word(-1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "cAPITALIZE Αβγword ");
        }

        #[test]
        fn forward_cursor_inside_of_word() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, new(Env), cx);

            // upcase word
            //  ^----
            env.current_buffer.get_mut().text.insert("upcase word");
            env.current_buffer.get_mut().text.set_cursor(2);
            upcase_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "upCASE word");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("DOWNCASE WORD");
            env.current_buffer.get_mut().text.set_cursor(2);
            downcase_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "DOwncase WORD");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("capitalize word");
            env.current_buffer.get_mut().text.set_cursor(2);
            capitalize_word(1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "caPitalize word");
        }

        #[test]
        fn backward_cursor_inside_of_word() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, new(Env), cx);

            // upcase word
            //        --^
            env.current_buffer.get_mut().text.insert("upcase word");
            env.current_buffer.get_mut().text.set_cursor(9);
            upcase_word(-1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "upcase WOrd");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("downcase WORD");
            env.current_buffer.get_mut().text.set_cursor(11);
            downcase_word(-1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "downcase woRD");
            env.current_buffer.get_mut().text = text_buffer::Buffer::default();
            env.current_buffer.get_mut().text.insert("capitalize word");
            env.current_buffer.get_mut().text.set_cursor(13);
            capitalize_word(-1, env, cx).unwrap();
            assert_eq!(env.current_buffer.get().text, "capitalize Word");
        }
    }
}
