//! String and character case conversion.
use anyhow::Result;
use std::ops::Range;

use crate::core::{gc::Rt, object::Object};
use crate::fns::StringOrChar;
use crate::{Context, Env};
use rune_macros::defun;
use rune_text::{BytePos, CharPos, Rope};

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
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    let range = if offset >= 0 {
        find_forward_word(text, read.point)?
    } else {
        find_backward_word(text, read.point)?
    };
    let selected = text.slice(CharPos(range.start)..CharPos(range.end))?;
    let source: String = selected.chars_from(CharPos(0))?.collect();
    let replacement = casify_string(&source, mode);
    env.editor_gateway().replace_preserving_properties(
        read.handle,
        read.epoch,
        range,
        &replacement,
        cx,
    )?;
    Ok(())
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

fn find_forward_word(buf: &Rope, cursor: usize) -> Result<Range<usize>> {
    let end = cursor
        + buf
            .chars_from(CharPos(cursor))?
            .enumerate()
            .skip_while(|(_, c)| !c.is_alphanumeric())
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| buf.len_chars() - cursor);
    Ok(cursor..end)
}
fn find_backward_word(buf: &Rope, cursor: usize) -> Result<Range<usize>> {
    let chunks = buf.chunks(BytePos(0)..buf.char_to_byte(CharPos(cursor))?)?;
    let start = cursor
        - chunks
            .rev()
            .flat_map(|s| s.chars().rev())
            .enumerate()
            .skip_while(|(_, c)| !c.is_alphanumeric())
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(0);
    Ok(start..cursor)
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

    mod upcase_word {
        use super::*;
        use rune_core::macros::root;
        use rune_editor::Document;
        fn setup(env: &mut Rt<Env>, text: &str, point: usize, cx: &Context) {
            let read = env.buffer_read(cx).unwrap();
            let g = env.editor_gateway();
            g.splice(read.handle, read.epoch, 0..read.document.text().len_chars(), text, cx)
                .unwrap();
            g.set_point(read.handle, point, cx).unwrap();
        }
        #[test]
        fn forward_and_inside_unicode_words() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, init(Env::new_isolated(cx).unwrap()), cx);
            setup(env, "αβγ word", 0, cx);
            upcase_word(1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "ΑΒΓ word");
            setup(env, "DOWNCASE WORD", 2, cx);
            downcase_word(1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "DOwncase WORD");
            setup(env, "αΒΓ wORD", 0, cx);
            capitalize_word(1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "Αβγ wORD");
            setup(env, "ΑΒΓ woRd", 0, cx);
            downcase_word(1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "αβγ woRd");
            setup(env, "upcase word", 2, cx);
            upcase_word(1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "upCASE word");
            setup(env, "capitalize word", 2, cx);
            capitalize_word(1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "caPitalize word");
        }
        #[test]
        fn backward_and_inside_unicode_words() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, init(Env::new_isolated(cx).unwrap()), cx);
            setup(env, "upcase αβγword ", 15, cx);
            upcase_word(-1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "upcase ΑΒΓWORD ");
            setup(env, "downcase WORD", 11, cx);
            downcase_word(-1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "downcase woRD");
            setup(env, "capitalize word", 13, cx);
            capitalize_word(-1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "capitalize Word");
            setup(env, "upcase word", 9, cx);
            upcase_word(-1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "upcase WOrd");
            setup(env, "dOwNcAsE αΒΓWord ", 17, cx);
            downcase_word(-1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "dOwNcAsE αβγword ");
            setup(env, "cAPITALIZE αΒΓWORD ", 19, cx);
            capitalize_word(-1, env, cx).unwrap();
            assert_eq!(env.buffer_read(cx).unwrap().document.to_text(), "cAPITALIZE Αβγword ");
        }
        #[test]
        fn expanding_case_updates_following_properties() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, init(Env::new_isolated(cx).unwrap()), cx);
            setup(env, "ß tail", 0, cx);
            let read = env.buffer_read(cx).unwrap();
            let g = env.editor_gateway();
            // SAFETY: integer immediate has no foreign reachable heap payload.
            let style = unsafe { g.intern_property(cx.add(42), cx) }.unwrap();
            let prepared = g.prepare_properties(&read, CharPos(2)..CharPos(6), &style, cx).unwrap();
            g.publish_properties(prepared, cx).unwrap();
            upcase_word(1, env, cx).unwrap();
            let read = env.buffer_read(cx).unwrap();
            assert_eq!(read.document.to_text(), "SS tail");
            assert_eq!(read.document.properties().at(CharPos(2)), Some(rune_text::PropSetId(0)));
            assert_eq!(
                read.document.properties().at(CharPos(3)),
                Some(rune_text::PropSetId(style.id()))
            );
        }
        #[test]
        fn same_width_case_keeps_position_properties() {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            root!(env, init(Env::new_isolated(cx).unwrap()), cx);
            setup(env, "αβ word", 0, cx);
            let read = env.buffer_read(cx).unwrap();
            let g = env.editor_gateway();
            // SAFETY: integer immediates carry no heap graph.
            let first = unsafe { g.intern_property(cx.add(1), cx) }.unwrap();
            let second = unsafe { g.intern_property(cx.add(2), cx) }.unwrap();
            let prepared = g
                .prepare_property_runs(
                    &read,
                    [
                        (CharPos(0)..CharPos(1), first.clone()),
                        (CharPos(1)..CharPos(2), second.clone()),
                    ],
                    cx,
                )
                .unwrap();
            g.publish_properties(prepared, cx).unwrap();
            let epoch = env.buffer_read(cx).unwrap().epoch;
            upcase_word(1, env, cx).unwrap();
            let after = env.buffer_read(cx).unwrap();
            assert_eq!(after.epoch, epoch + 1);
            assert_eq!(
                after.document.properties().at(CharPos(0)),
                Some(rune_text::PropSetId(first.id()))
            );
            assert_eq!(
                after.document.properties().at(CharPos(1)),
                Some(rune_text::PropSetId(second.id()))
            );
        }
    }
}
