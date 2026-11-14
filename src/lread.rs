//! Loading elisp from files and strings.
use crate::core::cons::Cons;
use crate::core::env::{Env, sym};
use crate::core::error::{Type, TypeError};
use crate::core::gc::{Context, Rt, Rto};
use crate::core::object::{
    Function, Gc, LispString, NIL, Object, ObjectType, OptionalFlag, Symbol, TRUE, TagType,
    WithLifetime,
};
use crate::reader;
use crate::{interpreter, rooted_iter};
use anyhow::{Context as _, anyhow};
use anyhow::{Result, bail, ensure};
use fallible_streaming_iterator::FallibleStreamingIterator;
use rune_core::macros::{call, rebind, root};
use rune_macros::defun;
use std::fs;
use std::path::{Path, PathBuf};

/// GNU validate_subarray uses character positions, including negative bounds,
/// and permits the empty interval at the end of a string.
fn read_string_bounds(start: Option<i64>, end: Option<i64>, len: usize) -> Result<(usize, usize)> {
    let len = i64::try_from(len).context("string length exceeds index range")?;
    let normalize = |index: i64| if index < 0 { len + index } else { index };
    let start = normalize(start.unwrap_or(0));
    let end = normalize(end.unwrap_or(len));
    ensure!(
        0 <= start && start <= end && end <= len,
        "read-from-string bounds {start}..{end} are out of range for string of length {len}"
    );
    Ok((start as usize, end as usize))
}

#[defun]
pub(crate) fn read_from_string<'ob>(
    string: &str,
    start: Option<i64>,
    end: Option<i64>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let (start, end) = read_string_bounds(start, end, string.chars().count())?;
    // The shared reader and loader use byte offsets internally; only this Lisp
    // boundary accepts and returns character positions (GNU lread.c:2511+).
    let start_byte = string.char_indices().nth(start).map_or(string.len(), |(pos, _)| pos);
    let end_byte = string.char_indices().nth(end).map_or(string.len(), |(pos, _)| pos);
    let input = &string[start_byte..end_byte];
    let (obj, new_pos) = match reader::read(input, cx) {
        Ok((obj, pos)) => (obj, pos),
        Err(mut e) => {
            // Keep reader diagnostics in their existing absolute byte units.
            e.update_pos(start_byte);
            bail!(e);
        }
    };
    let final_index = start + input[..new_pos].chars().count();
    Ok(Cons::new(obj, final_index as i64, cx).into())
}

pub(crate) fn load_internal(contents: &str, cx: &mut Context, env: &mut Rt<Env>) -> Result<bool> {
    let mut pos = 0;
    let macroexpand: Option<Function> = None;
    root!(macroexpand, cx);
    if let Some(fun) = sym::INTERNAL_MACROEXPAND_FOR_LOAD.func(cx) {
        macroexpand.set(Some(fun));
    }
    loop {
        let (obj, new_pos) = match reader::read(&contents[pos..], cx) {
            Ok((obj, pos)) => (obj, pos),
            Err(reader::Error::EmptyStream) => return Ok(true),
            Err(mut e) => {
                e.update_pos(pos);
                bail!(e);
            }
        };
        if crate::debug::debug_enabled() {
            let content = &contents[pos..(new_pos + pos)];
            println!("-----READ START-----\n {content}");
            println!("-----READ END-----");
        }
        root!(obj, cx);
        let result = if let Some(fun) = macroexpand.as_ref() {
            eager_expand(obj, fun, env, cx)
        } else {
            interpreter::eval(obj, None, env, cx)
        };
        if let Err(e) = result {
            let content = &contents[pos..(new_pos + pos)];
            println!("-----LOAD ERROR START-----\n {content}");
            println!("-----LOAD ERROR END-----");
            return Err(e);
        }
        assert_ne!(new_pos, 0);
        pos += new_pos;
    }
}

fn eager_expand<'ob>(
    obj: &Rto<Object>,
    macroexpand: &Rto<Function>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>, anyhow::Error> {
    let name = "internal-macroexpand-for-load";
    let val = call!(macroexpand, obj, NIL; name, env, cx)?;
    let val = rebind!(val, cx);
    if let Ok((sym::PROGN, forms)) = val.as_cons_pair() {
        root!(val, NIL, cx);
        rooted_iter!(forms, forms.tag(), cx);
        while let Some(form) = forms.next()? {
            let result = eager_expand(form, macroexpand, env, cx)?;
            val.set(result);
        }
        return Ok(val.bind(cx));
    }
    let result = call!(macroexpand, val, TRUE; name, env, cx)?;
    root!(result, cx);
    interpreter::eval(result, None, env, cx)
}

fn file_in_path(file: &str, path: &str) -> Option<PathBuf> {
    let path = Path::new(path).join(file);
    // GNU openp appends suffixes to the whole name; with_extension would
    // incorrectly turn package.extra into package.el. This source-only loader
    // tries .el before the unsuffixed name; compiled/module suffixes remain open.
    let mut source_name = path.as_os_str().to_os_string();
    source_name.push(".el");
    let source = PathBuf::from(source_name);
    if source.is_file() { Some(source) } else { path.is_file().then_some(path) }
}

fn find_file_in_load_path(file: &str, cx: &Context, env: &Rt<Env>) -> Result<PathBuf> {
    let load_path = env.vars.get(sym::LOAD_PATH).context("`load-path' is unbound")?;
    let load_path = load_path.bind(cx);
    // GNU openp also searches the default directory for an empty load-path.
    if Path::new(file).is_absolute() || load_path.is_nil() {
        return file_in_path(file, "")
            .ok_or_else(|| anyhow!("Unable to find file `{file}' in load-path"));
    }
    let paths = load_path.as_list().context("`load-path' was not a list")?;
    for path in paths {
        let found = match path?.untag() {
            ObjectType::String(path) => file_in_path(file, path),
            // A nil entry denotes the default directory. Rune currently uses
            // the process directory, pending buffer default-directory support.
            ObjectType::NIL => file_in_path(file, ""),
            x => {
                return Err(TypeError::new(Type::String, x))
                    .context("Found non-string/non-nil in `load-path'");
            }
        };
        if let Some(found) = found {
            return Ok(found);
        }
    }
    Err(anyhow!("Unable to find file `{file}' in load-path"))
}

#[defun]
pub(crate) fn load(
    file: &Rto<Gc<&LispString>>,
    noerror: OptionalFlag,
    nomessage: OptionalFlag,
    cx: &mut Context,
    env: &mut Rt<Env>,
) -> Result<bool> {
    let noerror = noerror.is_some();
    let nomessage = nomessage.is_some();
    let file: &str = file.untag(cx);
    let final_file = match find_file_in_load_path(file, cx, env) {
        Ok(path) => path,
        Err(error) => return if noerror { Ok(false) } else { Err(error) },
    };

    let filename = String::from(file);
    if !nomessage {
        println!("Loading {filename}...");
    }
    let new_load_file = cx.add(final_file.to_string_lossy().to_string());
    let prev_load_file = match env.vars.get_mut(sym::LOAD_FILE_NAME) {
        Some(val) => {
            let prev = val.bind(cx);
            val.set(new_load_file);
            prev
        }
        None => NIL,
    };
    root!(prev_load_file, cx);
    let result = match fs::read_to_string(&final_file)
        .with_context(|| format!("Couldn't open file {:?}", final_file.as_os_str()))
    {
        Ok(content) => load_internal(&content, cx, env),
        Err(e) => match noerror {
            true => Ok(false),
            false => Err(e),
        },
    };

    if !nomessage && result.is_ok() {
        println!("Loading {filename} Done");
    }
    env.vars.insert(sym::LOAD_FILE_NAME, &*prev_load_file);
    result
}

#[defun]
pub(crate) fn intern<'ob>(string: &str, cx: &'ob Context) -> Symbol<'ob> {
    crate::core::env::intern(string, cx)
}

#[defun]
pub(crate) fn intern_soft(string: Object, obarray: OptionalFlag) -> Result<Symbol> {
    ensure!(obarray.is_none(), "intern-soft obarray not implemented");
    match string.untag() {
        ObjectType::Symbol(sym) => {
            if sym.interned() {
                Ok(sym)
            } else {
                Ok(sym::NIL)
            }
        }
        ObjectType::String(string) => {
            let map = crate::core::env::INTERNED_SYMBOLS.lock().unwrap();
            match map.get(string) {
                Some(sym) => Ok(unsafe { sym.with_lifetime() }),
                None => Ok(sym::NIL),
            }
        }
        x => Err(TypeError::new(Type::String, x).into()),
    }
}

defsym!(INTERNAL_MACROEXPAND_FOR_LOAD);
defvar!(LEXICAL_BINDING, true);
defvar!(CURRENT_LOAD_LIST);
defvar!(LOAD_HISTORY);
defvar!(LOAD_PATH, list![format!("{}/lisp", env!("CARGO_MANIFEST_DIR"))]);
defvar!(LOAD_FILE_NAME);
defvar!(BYTE_BOOLEAN_VARS);
defvar!(MACROEXP__DYNVARS);
defvar!(AFTER_LOAD_ALIST);

#[cfg(test)]
mod test {

    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::{list, root};

    #[test]
    fn read_from_string_returns_absolute_character_indices() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, start, end, expected_value, expected_position) in [
            ("xx 42", Some(3), None, 42_i64, 5_i64),
            ("xx 42 tail", Some(3), Some(5), 42, 5),
            ("\u{e9} 42", Some(2), None, 42, 4),
            ("\u{e9} 42 tail", Some(2), Some(4), 42, 4),
            ("\u{e9} 42 tail", Some(-7), Some(-5), 42, 4),
            ("xx 42", Some(-2), Some(5), 42, 5),
        ] {
            let result = read_from_string(source, start, end, &cx).unwrap();
            let ObjectType::Cons(pair) = result.untag() else {
                panic!("expected reader pair");
            };
            let (value, position) = (pair.car(), pair.cdr());
            assert_eq!(value, cx.add(expected_value), "{source}");
            assert_eq!(position, cx.add(expected_position), "{source}");
        }
        let result = read_from_string("xx \"\u{e9}\u{1f600}\" tail", Some(3), None, &cx).unwrap();
        let ObjectType::Cons(pair) = result.untag() else {
            panic!("expected reader pair");
        };
        let (value, position) = (pair.car(), pair.cdr());
        assert_eq!(value, cx.add("\u{e9}\u{1f600}"));
        assert_eq!(position, cx.add(7_i64));
    }

    #[test]
    fn read_from_string_bounds_do_not_panic_on_unicode_or_empty_slices() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        // Character 1 is after the complete multibyte scalar, not in its UTF-8 encoding.
        let result = read_from_string("\u{e9}42", Some(1), Some(3), &cx).unwrap();
        let ObjectType::Cons(pair) = result.untag() else {
            panic!("expected reader pair");
        };
        let (value, position) = (pair.car(), pair.cdr());
        assert_eq!(value, cx.add(42_i64));
        assert_eq!(position, cx.add(3_i64));
        for (source, start, end) in [
            ("", None, None),
            ("abc", Some(3), None),
            ("abc", Some(1), Some(1)),
            ("\u{e9}", Some(1), Some(1)),
        ] {
            let error = read_from_string(source, start, end, &cx).unwrap_err();
            assert_eq!(error.downcast_ref::<reader::Error>(), Some(&reader::Error::EmptyStream));
        }
        for (start, end) in [
            (Some(2), Some(1)),
            (Some(4), None),
            (Some(-4), None),
            (None, Some(4)),
            (None, Some(-4)),
            (Some(i64::MIN), None),
        ] {
            assert!(read_from_string("\u{e9}42", start, end, &cx).is_err());
        }
    }

    struct LoadFixture(PathBuf);

    impl LoadFixture {
        fn new() -> Self {
            Self::in_directory(&std::env::temp_dir())
        }

        fn in_directory(base: &Path) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = base.join(format!(
                "rune-load-search-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for LoadFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn source_suffix_is_appended_and_precedes_bare_file() {
        let fixture = LoadFixture::new();
        let directory = fixture.0.to_str().unwrap();
        fs::write(fixture.0.join("package.el"), "wrong target").unwrap();
        assert_eq!(file_in_path("package.extra", directory), None);
        fs::write(fixture.0.join("package.extra.el"), "right source").unwrap();
        fs::write(fixture.0.join("package.extra"), "bare fallback").unwrap();
        assert_eq!(
            file_in_path("package.extra", directory),
            Some(fixture.0.join("package.extra.el"))
        );
        fs::remove_file(fixture.0.join("package.extra.el")).unwrap();
        assert_eq!(file_in_path("package.extra", directory), Some(fixture.0.join("package.extra")));
        fs::create_dir(fixture.0.join("only-directory")).unwrap();
        assert_eq!(file_in_path("only-directory", directory), None);
    }

    #[test]
    fn load_path_search_handles_nil_and_respects_directory_order() {
        let fixture = LoadFixture::new();
        fs::create_dir(fixture.0.join("first")).unwrap();
        fs::create_dir(fixture.0.join("second")).unwrap();
        fs::write(fixture.0.join("first/ordered.el"), "first").unwrap();
        fs::write(fixture.0.join("second/ordered.el"), "second").unwrap();
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        let path = crate::core::env::intern("load-path", cx);
        let directories = list!(fixture.0.join("first").to_str().unwrap(), fixture.0.join("second").to_str().unwrap(); cx);
        env.vars.insert(path, directories);
        assert_eq!(
            find_file_in_load_path("ordered", cx, env).unwrap(),
            fixture.0.join("first/ordered.el")
        );
        let current_fixture = LoadFixture::in_directory(&std::env::current_dir().unwrap());
        fs::write(current_fixture.0.join("current.el"), "current directory").unwrap();
        let relative = Path::new(current_fixture.0.file_name().unwrap()).join("current");
        for directories in [NIL, list!(NIL; cx)] {
            env.vars.insert(path, directories);
            assert_eq!(
                find_file_in_load_path(relative.to_str().unwrap(), cx, env).unwrap(),
                relative.with_extension("el")
            );
        }
        let absolute = fixture.0.join("second/ordered");
        for directories in [NIL, list!(NIL; cx)] {
            env.vars.insert(path, directories);
            assert_eq!(
                find_file_in_load_path(absolute.to_str().unwrap(), cx, env).unwrap(),
                fixture.0.join("second/ordered.el")
            );
        }
    }

    #[test]
    fn executable_lisp_headers_load_without_evaluating_the_header() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        load_internal("#!/usr/bin/env rune\n(setq shebang-loaded-value 41)\n#! ignored\n(setq shebang-loaded-value (+ shebang-loaded-value 1))", cx, env).unwrap();
        let form = reader::read("shebang-loaded-value", cx).unwrap().0;
        root!(form, cx);
        let value = interpreter::eval(form, None, env, cx).unwrap();
        assert_eq!(value, 42_i64);
    }

    #[test]
    fn test_load() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        load_internal("(setq foo 1) (setq bar 2) (setq baz 1.5)", cx, env).unwrap();

        let obj = reader::read("(+ foo bar baz)", cx).unwrap().0;
        root!(obj, cx);
        let val = interpreter::eval(obj, None, env, cx).unwrap();
        assert_eq!(val, 4.5);
    }
}
