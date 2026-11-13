//! Search utilities.
use crate::core::{
    env::Env,
    gc::{Context, Rt},
    object::{List, NIL, Object, ObjectType, OptionalFlag},
};
use anyhow::{Result, bail, ensure};
use fallible_iterator::FallibleIterator;
use fancy_regex::Regex;
use rune_macros::defun;

#[defun]
fn string_match<'ob>(
    regexp: &str,
    string: &str,
    start: Option<i64>,
    _inhibit_modify: OptionalFlag,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    // TODO: implement inhibit-modify
    let re = Regex::new(&lisp_regex_to_rust(regexp))?;

    let char_len = string.chars().count();
    let start = start.unwrap_or(0);
    let start = if start < 0 { char_len as i128 + start as i128 } else { start as i128 };
    ensure!(start >= 0 && start <= char_len as i128, "string-match start out of range");
    let start = start as usize;
    let start_byte = char_byte_offset(string, start)?;
    if let Some(matches) = re.captures_from_pos(string, start_byte)? {
        let mut all: Vec<Object> = Vec::new();
        for group in matches.iter() {
            if let Some(group) = group {
                all.push(string[..group.start()].chars().count().into());
                all.push(string[..group.end()].chars().count().into());
            } else {
                all.push(NIL);
                all.push(NIL);
            }
        }
        let result = all.first().copied().unwrap_or(NIL);
        env.match_data.set(crate::fns::slice_into_list(&all, None, cx));
        Ok(result)
    } else {
        Ok(NIL)
    }
}

// Lisp string positions count characters, whereas Rust slices use byte offsets.
fn char_byte_offset(string: &str, position: usize) -> Result<usize> {
    if let Some((offset, _)) = string.char_indices().nth(position) {
        return Ok(offset);
    }
    ensure!(position == string.chars().count(), "string position out of range");
    Ok(string.len())
}

#[defun]
fn replace_match(
    newtext: &str,
    _fixedcase: OptionalFlag,
    _literal: OptionalFlag,
    string: Option<&str>,
    subexp: Option<usize>,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<String> {
    // TODO: Handle newtext interpolation. Treat \ as special. See docstring for more.
    //
    // TODO: Handle automatic case adjustment
    let Some(string) = string else { bail!("replace-match for buffers not yet implemented") };
    let mut match_data = env.match_data.bind(cx).as_list()?.fallible();
    let subexp = subexp.unwrap_or(0);
    let sub_err = || format!("replace-match subexpression {subexp} does not exist");
    for _ in 0..subexp.checked_mul(2).ok_or_else(|| anyhow::anyhow!(sub_err()))? {
        ensure!(match_data.next()?.is_some(), sub_err());
    }
    let Some(beg) = match_data.next()? else { bail!(sub_err()) };
    let Some(end) = match_data.next()? else { bail!(sub_err()) };

    let beg: usize = beg.try_into()?;
    let end: usize = end.try_into()?;
    ensure!(beg <= end, "replace-match invalid match bounds");
    let beg = char_byte_offset(string, beg)?;
    let end = char_byte_offset(string, end)?;

    // replace the range beg..end in string with newtext
    let mut new_string = String::new();
    new_string.push_str(&string[..beg]);
    new_string.push_str(newtext);
    new_string.push_str(&string[end..]);
    Ok(new_string)
}

#[defun]
fn regexp_quote(string: &str) -> String {
    let mut quoted = String::new();
    for ch in string.chars() {
        if let '[' | '*' | '.' | '\\' | '?' | '+' | '^' | '$' = ch {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted
}

pub(crate) fn lisp_regex_to_rust(regexp: &str) -> String {
    let mut norm_regex = String::new();
    let mut chars = regexp.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            // Invert the escaping of parens. i.e. \( => ( and ( => \(
            '(' | ')' | '{' | '}' => {
                norm_regex.push('\\');
                norm_regex.push(ch);
            }
            '\\' => match chars.next() {
                Some((_, c @ '('..=')' | c @ '{' | c @ '}')) => norm_regex.push(c),
                Some((_, '`')) => norm_regex += "\\A",
                Some((_, '\'')) => norm_regex += "\\z",
                Some((_, c)) => {
                    norm_regex.push('\\');
                    norm_regex.push(c);
                }
                None => norm_regex.push('\\'),
            },
            '[' => {
                let word = "[:word:]";
                if regexp[idx..].starts_with(word) {
                    chars.nth(word.len() - 2);
                    norm_regex.push_str("a-zA-Z");
                } else {
                    norm_regex.push('[');
                }
            }
            c => norm_regex.push(c),
        }
    }
    norm_regex
}

#[defun]
fn match_data<'ob>(
    integer: OptionalFlag,
    reuse: OptionalFlag,
    reseat: OptionalFlag,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    ensure!(integer.is_none(), "match-data integer field is not implemented");
    ensure!(reuse.is_none(), "match-data reuse field is not implemented");
    ensure!(reseat.is_none(), "match-data reseat field is not implemented");
    Ok(env.match_data.bind(cx))
}

#[defun]
fn set_match_data<'ob>(list: List, _reseat: OptionalFlag, env: &mut Rt<Env>) -> Object<'ob> {
    // TODO: add reseat when markers implemented
    let obj: Object = list.into();
    env.match_data.set(obj);
    NIL
}

#[defun]
fn match_beginning<'ob>(subexp: usize, env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    let list = env.match_data.bind(cx).as_list()?;
    let index = subexp
        .checked_mul(2)
        .ok_or_else(|| anyhow::anyhow!("subexpression out of range"))?;
    Ok(list.fallible().nth(index)?.unwrap_or_default())
}

#[defun]
fn match_end<'ob>(subexp: usize, env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    let list = env.match_data.bind(cx).as_list()?;
    let index = subexp
        .checked_mul(2)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("subexpression out of range"))?;
    Ok(list.fallible().nth(index)?.unwrap_or_default())
}

#[defun]
#[expect(non_snake_case)]
fn match_data__translate(n: i64, env: &Rt<Env>, cx: &Context) -> Result<()> {
    let search_regs: List = env.match_data.bind(cx).try_into()?;
    let mut updates = Vec::new();
    for reg in search_regs.conses() {
        let reg = reg?;
        let old = reg.car();
        if old == NIL {
            continue;
        }
        let ObjectType::Int(position) = old.untag() else {
            bail!("match data was not int");
        };
        let translated = position
            .checked_add(n)
            .ok_or_else(|| anyhow::anyhow!("match position overflow"))?;
        // Validate mutability without changing the value before committing any updates.
        reg.set_car(old)?;
        updates.push((reg, translated));
    }
    for (reg, translated) in updates {
        reg.set_car(translated.into())?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    use super::*;

    #[test]
    fn lisp_regex() {
        assert_eq!(lisp_regex_to_rust("foo"), "foo");
        assert_eq!(lisp_regex_to_rust("\\foo"), "\\foo");
        assert_eq!(lisp_regex_to_rust("\\(foo\\)"), "(foo)");
        assert_eq!(lisp_regex_to_rust("(foo)"), "\\(foo\\)");
        assert_eq!(lisp_regex_to_rust("\\`"), "\\A");
        assert_eq!(lisp_regex_to_rust("\\'"), "\\z");
        assert_eq!(lisp_regex_to_rust("[[:word:]]"), "[a-zA-Z]");
        assert_eq!(lisp_regex_to_rust("[[:word:]_]"), "[a-zA-Z_]");
    }

    #[test]
    fn character_offsets_and_bounds() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        let position: usize =
            string_match("b", "éab", Some(1), None, env, cx).unwrap().try_into().unwrap();
        assert_eq!(position, 2);
        assert_eq!(replace_match("X", None, None, Some("éab"), None, env, cx).unwrap(), "éaX");
        let position: usize =
            string_match("b", "éab", Some(-1), None, env, cx).unwrap().try_into().unwrap();
        assert_eq!(position, 2);
        assert!(string_match("", "é", Some(-2), None, env, cx).is_err());
        assert!(string_match("", "é", Some(2), None, env, cx).is_err());
        assert!(string_match("", "é", Some(i64::MIN), None, env, cx).is_err());
        assert!(string_match("", "é", Some(i64::MAX), None, env, cx).is_err());
    }

    #[test]
    fn unmatched_groups_keep_capture_indices() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        string_match("\\(a\\)?\\(b\\)", "éb", None, None, env, cx).unwrap();
        assert_eq!(match_beginning(1, env, cx).unwrap(), NIL);
        let beginning: usize = match_beginning(2, env, cx).unwrap().try_into().unwrap();
        let end: usize = match_end(2, env, cx).unwrap().try_into().unwrap();
        assert_eq!((beginning, end), (1, 2));
        assert_eq!(replace_match("X", None, None, Some("éb"), Some(2), env, cx).unwrap(), "éX");
        assert!(match_beginning(usize::MAX, env, cx).is_err());
        assert!(match_end(usize::MAX, env, cx).is_err());
        assert!(replace_match("X", None, None, Some("éb"), Some(usize::MAX), env, cx).is_err());
    }

    #[test]
    fn start_keeps_anchor_and_lookbehind_context() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        assert_eq!(string_match("^b", "ab", Some(1), None, env, cx).unwrap(), NIL);
        let pos: usize = string_match("\\(?<=é\\)b", "éb", Some(1), None, env, cx)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(pos, 1);
    }

    #[test]
    fn translation_skips_nil_and_preflights_errors() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        string_match("\\(a\\)?b", "éb", None, None, env, cx).unwrap();
        match_data__translate(2, env, cx).unwrap();
        let pos: usize = match_beginning(0, env, cx).unwrap().try_into().unwrap();
        assert_eq!(pos, 3);
        assert_eq!(match_beginning(1, env, cx).unwrap(), NIL);
        env.match_data
            .set(crate::fns::slice_into_list(&[1usize.into(), true.into()], None, cx));
        assert!(match_data__translate(2, env, cx).is_err());
        let pos: usize = match_beginning(0, env, cx).unwrap().try_into().unwrap();
        assert_eq!(pos, 1);
    }

    #[test]
    fn invalid_replacement_bounds_signal() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        for (beg, end) in [(2usize, 1usize), (0, 10)] {
            env.match_data
                .set(crate::fns::slice_into_list(&[beg.into(), end.into()], None, cx));
            assert!(replace_match("X", None, None, Some("é"), None, env, cx).is_err());
        }
    }

    #[test]
    fn test_replace_match() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, new(Env), cx);
        let string = "foo bar baz";
        let newtext = "quux";
        string_match("bar", string, None, None, env, cx).unwrap();
        let result = replace_match(newtext, None, None, Some(string), None, env, cx).unwrap();
        assert_eq!(result, "foo quux baz");
    }
}
