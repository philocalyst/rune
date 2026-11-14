//! Loading elisp from files and strings.
use crate::core::cons::Cons;
use crate::core::env::{Env, sym};
use crate::core::error::{Type, TypeError};
use crate::core::gc::{Context, Rt, Rto, Slot};
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
    root!(declarations, new(Vec<Slot<Symbol>>), cx);
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
            eager_expand(obj, fun, declarations, env, cx)
        } else {
            interpreter::eval_loaded(obj, declarations, env, cx)
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

fn eager_expand<'ob, 'rt>(
    obj: &Rto<Object>,
    macroexpand: &Rto<Function>,
    declarations: &mut Rt<Vec<Slot<Symbol<'rt>>>>,
    env: &mut Rt<Env<'rt>>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>, anyhow::Error> {
    let name = "internal-macroexpand-for-load";
    let val = call!(macroexpand, obj, NIL; name, env, cx)?;
    let val = rebind!(val, cx);
    if let Ok((sym::PROGN, forms)) = val.as_cons_pair() {
        root!(val, NIL, cx);
        rooted_iter!(forms, forms.tag(), cx);
        while let Some(form) = forms.next()? {
            let result = eager_expand(form, macroexpand, declarations, env, cx)?;
            val.set(result);
        }
        return Ok(val.bind(cx));
    }
    let result = call!(macroexpand, val, TRUE; name, env, cx)?;
    root!(result, cx);
    interpreter::eval_loaded(result, declarations, env, cx)
}

fn file_in_path(file: &str, path: &str) -> Option<PathBuf> {
    file_in_path_options(file, path, false, false)
}
fn file_in_path_options(
    file: &str,
    directory: &str,
    no_suffix: bool,
    must_suffix: bool,
) -> Option<PathBuf> {
    let path = Path::new(directory).join(file);
    if no_suffix {
        return path.is_file().then_some(path);
    }
    let mut source_name = path.as_os_str().to_os_string();
    source_name.push(".el");
    let source = PathBuf::from(source_name);
    if source.is_file() {
        return Some(source);
    }
    // GNU ignores MUST-SUFFIX for explicit directory names. NOSUFFIX above
    // also takes precedence, as qualified by direct GNU load probes.
    let directory_name = Path::new(file).parent().is_some_and(|p| !p.as_os_str().is_empty());
    let recognized = [".el", ".elc", ".so", ".dll", ".dylib"]
        .iter()
        .any(|suffix| file.ends_with(suffix));
    if must_suffix && !directory_name && !recognized {
        return None;
    }
    path.is_file().then_some(path)
}
fn search_load_path(
    file: &str,
    cx: &Context,
    env: &Rt<Env>,
    no_suffix: bool,
    must_suffix: bool,
) -> Result<Option<PathBuf>> {
    if Path::new(file).is_absolute() {
        return Ok(file_in_path_options(file, "", no_suffix, must_suffix));
    }
    let load_path = env.vars.get(sym::LOAD_PATH).context("`load-path' is unbound")?.bind(cx);
    if load_path.is_nil() {
        return Ok(file_in_path_options(file, "", no_suffix, must_suffix));
    }
    // GNU ignores non-string entries and non-list/improper tails. Keep the
    // spine walk bounded instead of recursively printing a malformed value.
    let mut tail = load_path;
    let mut seen = std::collections::HashSet::new();
    while let ObjectType::Cons(pair) = tail.untag() {
        ensure!(seen.len() < 100_000, "load-path exceeds traversal budget");
        seen.try_reserve(1)?;
        if !seen.insert(tail.into_raw().addr()) {
            break;
        }
        let found = match pair.car().untag() {
            ObjectType::String(path) => file_in_path_options(file, path, no_suffix, must_suffix),
            ObjectType::NIL => file_in_path_options(file, "", no_suffix, must_suffix),
            _ => None,
        };
        if found.is_some() {
            return Ok(found);
        }
        tail = pair.cdr();
    }
    Ok(None)
}
fn find_file_in_load_path(file: &str, cx: &Context, env: &Rt<Env>) -> Result<PathBuf> {
    search_load_path(file, cx, env, false, false)?
        .ok_or_else(|| anyhow!("Unable to find file `{file}' in load-path"))
}

/// Native Rust callers keep their existing three-argument load semantics.
pub(crate) fn load(
    file: &Rto<Gc<&LispString>>,
    noerror: OptionalFlag,
    nomessage: OptionalFlag,
    cx: &mut Context,
    env: &mut Rt<Env>,
) -> Result<bool> {
    load_with_options(file, noerror, nomessage, false, false, cx, env)
}

#[defun(name = "load")]
fn load_lisp(
    file: &Rto<Gc<&LispString>>,
    noerror: OptionalFlag,
    nomessage: OptionalFlag,
    nosuffix: OptionalFlag,
    must_suffix: OptionalFlag,
    cx: &mut Context,
    env: &mut Rt<Env>,
) -> Result<bool> {
    load_with_options(file, noerror, nomessage, nosuffix.is_some(), must_suffix.is_some(), cx, env)
}
fn load_with_options(
    file: &Rto<Gc<&LispString>>,
    noerror: OptionalFlag,
    nomessage: OptionalFlag,
    no_suffix: bool,
    must_suffix: bool,
    cx: &mut Context,
    env: &mut Rt<Env>,
) -> Result<bool> {
    let noerror = noerror.is_some();
    let nomessage = nomessage.is_some();
    let file: &str = file.untag(cx);
    let final_file = match search_load_path(file, cx, env, no_suffix, must_suffix)? {
        Some(path) => path,
        None if noerror => return Ok(false),
        None => bail!("Unable to find file `{file}' in load-path"),
    };
    ensure!(
        !matches!(
            final_file.extension().and_then(|x| x.to_str()),
            Some("elc" | "eln" | "so" | "dll" | "dylib")
        ),
        "Compiled Lisp and native module loading are not implemented"
    );

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
    let result = match fs::read_to_string(&final_file) {
        Ok(content) => load_internal(&content, cx, env),
        Err(error) if noerror && error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("Couldn't open file {:?}", final_file.as_os_str()))
        }
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
    #[test]
    fn source_load_flags_match_gnu_suffix_order_and_precedence() {
        let fixture = LoadFixture::new();
        fs::write(fixture.0.join("a"), "(setq rune-load-marker 1)").unwrap();
        fs::write(fixture.0.join("b.el"), "(setq rune-load-marker 2)").unwrap();
        fs::write(fixture.0.join("b.el.el"), "(setq rune-load-marker 3)").unwrap();
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let path = list!(fixture.0.to_str().unwrap(); cx);
        env.vars.insert(sym::LOAD_PATH, path);
        for (file, no_suffix, must_suffix, expected) in [
            ("a", false, false, 1),
            ("a", true, false, 1),
            ("a", false, true, 0),
            ("a", true, true, 1),
            ("b.el", false, false, 3),
            ("b.el", true, false, 2),
            ("b.el", false, true, 3),
            ("b.el", true, true, 2),
            ("b", false, true, 2),
            ("b", true, true, 0),
        ] {
            env.vars
                .insert(crate::core::env::intern("rune-load-marker", cx), Object::from(0));
            let file = cx.add(file);
            let file: Gc<&LispString> = file.try_into().unwrap();
            root!(file, cx);
            let loaded = load_lisp(
                file,
                Some(()),
                Some(()),
                no_suffix.then_some(()),
                must_suffix.then_some(()),
                cx,
                env,
            )
            .unwrap();
            assert_eq!(loaded, expected != 0);
            assert_eq!(
                env.vars.get(crate::core::env::intern("rune-load-marker", cx)).unwrap().bind(cx),
                Object::from(expected)
            );
        }
        let absolute = fixture.0.join("a");
        let file = cx.add(absolute.to_str().unwrap());
        let file: Gc<&LispString> = file.try_into().unwrap();
        root!(file, cx);
        assert!(load_lisp(file, None, Some(()), None, Some(()), cx, env).unwrap());
        let source = crate::reader::read("(func-arity 'load)", cx).unwrap().0;
        root!(source, cx);
        assert_eq!(
            format!("{}", crate::interpreter::eval(source, None, env, cx).unwrap()),
            "(1 . 5)"
        );
    }
    #[test]
    fn load_noerror_does_not_hide_execution_encoding_errors_but_ignores_bad_path_entries() {
        let fixture = LoadFixture::new();
        fs::write(fixture.0.join("bad.el"), "(error \"actual load error\")").unwrap();
        fs::write(fixture.0.join("invalid.el"), [255]).unwrap();
        fs::write(fixture.0.join("compiled.elc"), "nil").unwrap();
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        env.vars.insert(sym::LOAD_PATH, list!(fixture.0.to_str().unwrap();cx));
        for name in ["bad", "invalid", "compiled.elc"] {
            let file = cx.add(name);
            let file: Gc<&LispString> = file.try_into().unwrap();
            root!(file, cx);
            assert!(load_lisp(file, Some(()), Some(()), None, None, cx, env).is_err());
        }
        let file = cx.add("rune-load-no-such-file-for-path-errors");
        let file: Gc<&LispString> = file.try_into().unwrap();
        root!(file, cx);
        root!(paths, new(Vec<Slot<Object>>), cx);
        paths.push(Object::from(17));
        paths.push(list!(17;cx));
        paths.push(list!(TRUE;cx));
        paths.push(Object::from(Cons::new("/tmp", 17, cx)));
        let cycle = Cons::new(17, NIL, cx);
        cycle.set_cdr(cycle.into()).unwrap();
        paths.push(Object::from(cycle));
        for index in 0..paths.len() {
            env.vars.insert(sym::LOAD_PATH, paths[index].bind(cx));
            assert!(!load_lisp(file, Some(()), Some(()), None, None, cx, env).unwrap());
            let error = load_lisp(file, None, Some(()), None, None, cx, env).unwrap_err();
            assert!(error.to_string().contains("Unable to find file"));
        }
        // A bad entry does not stop searching later valid directories.
        env.vars.insert(sym::LOAD_PATH, list!(17, fixture.0.to_str().unwrap();cx));
        let file = cx.add("bad");
        let file: Gc<&LispString> = file.try_into().unwrap();
        root!(file, cx);
        assert!(load_lisp(file, Some(()), Some(()), None, None, cx, env).is_err());
    }
}

#[cfg(test)]
mod file_declaration_tests {
    use super::*;
    use crate::core::{
        env::{init_variables, intern},
        gc::RootSet,
    };
    #[test]
    fn declarations_persist_across_forms_but_not_across_load_calls() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        init_variables(cx, env);
        load_internal("(defvar vm-file-args)\n(garbage-collect)\n(defalias 'vm-file-reader (function (lambda () (car vm-file-args))))\n(defalias 'vm-file-call (function (lambda (args) (let ((vm-file-args args)) (vm-file-reader)))))\n(setq vm-file-observed (vm-file-call '(7)))", cx, env).unwrap();
        assert_eq!(env.vars.get(intern("vm-file-observed", cx)).unwrap().bind(cx), Object::from(7));
        assert!(!intern("vm-file-args", cx).is_special());
        load_internal("(setq vm-file-isolated (let ((vm-file-args '(9))) (condition-case nil (vm-file-reader) (void-variable 'isolated))))", cx, env).unwrap();
        assert_eq!(
            env.vars.get(intern("vm-file-isolated", cx)).unwrap().bind(cx),
            Object::from(intern("isolated", cx))
        );
    }
    #[test]
    fn eager_expanded_progn_shares_file_declarations() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        init_variables(cx, env);
        let function_form =
            reader::read("(function (lambda (form &optional final) form))", cx).unwrap().0;
        root!(function_form, cx);
        let function: Function =
            interpreter::eval(function_form, None, env, cx).unwrap().try_into().unwrap();
        root!(function, cx);
        root!(declarations, new(Vec<Slot<Symbol>>), cx);
        let form = reader::read("(progn (defvar vm-eager-args) (garbage-collect) (defalias 'vm-eager-reader (function (lambda () vm-eager-args))) (setq vm-eager-result (let ((vm-eager-args 17)) (vm-eager-reader))))", cx).unwrap().0;
        root!(form, cx);
        eager_expand(form, function, declarations, env, cx).unwrap();
        assert_eq!(env.vars.get(intern("vm-eager-result", cx)).unwrap().bind(cx), Object::from(17));
        assert!(!intern("vm-eager-args", cx).is_special());
    }
    #[test]
    fn nested_load_keeps_its_declarations_out_of_callers_scope() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        init_variables(cx, env);
        let directory =
            std::env::temp_dir().join(format!("rune-file-declarations-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("inner.el");
        fs::write(&path, "(defvar vm-inner-args)\n(defalias 'vm-inner-reader (function (lambda () vm-inner-args)))").unwrap();
        let source = format!(
            "(load {:?})\n(setq vm-inner-isolated (let ((vm-inner-args 9)) (condition-case nil (vm-inner-reader) (void-variable 'isolated))))",
            path.to_str().unwrap()
        );
        let result = load_internal(&source, cx, env);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&directory).unwrap();
        result.unwrap();
        assert_eq!(
            env.vars.get(intern("vm-inner-isolated", cx)).unwrap().bind(cx),
            Object::from(intern("isolated", cx))
        );
    }
}
