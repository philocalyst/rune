//! File I/O.
use crate::{
    core::{
        cons::Cons,
        env::{ArgSlice, Env, sym},
        error::{Type, TypeError},
        gc::{Context, Rt},
        object::{Number, Object, ObjectType, OptionalFlag, Symbol},
    },
    data::symbol_value,
    lisp::maybe_quit,
    search::lisp_regex_to_rust,
    sym::{FILE_NAME_HANDLER_ALIST, INHIBIT_FILE_NAME_HANDLERS, NIL},
};
use anyhow::{Result, bail, ensure};
use fancy_regex::Regex;
use rune_core::macros::list;
use rune_macros::defun;
use std::path::{Component, MAIN_SEPARATOR, Path};

/// Bounded local-file search for source loaders. No callbacks or file handlers.
#[defun]
fn locate_file_internal(
    filename: &str,
    path: Object,
    suffixes: Option<Object>,
    predicate: OptionalFlag,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<Option<String>> {
    ensure!(predicate.is_none(), "locate-file-internal custom predicates/access modes are not implemented");
    ensure!(!filename.as_bytes().contains(&0), "Filename contains NUL");
    ensure!(filename.len() <= 65536, "locate-file-internal filename exceeds limit");
    let mut directories = Vec::new();
    for entry in path.as_list()? {
        ensure!(directories.len() < 1024, "locate-file-internal path exceeds limit");
        let entry = entry?;
        directories.push(match entry.untag() {
            ObjectType::NIL => None,
            ObjectType::String(value) => {
                ensure!(value.len() <= 65536 && !value.as_bytes().contains(&0), "Invalid locate-file-internal directory");
                Some(String::from(&**value))
            },
            _ => return Err(TypeError::new(Type::String, entry).into()),
        });
    }
    if directories.is_empty() { directories.push(None); }
    let mut endings = Vec::new();
    for entry in suffixes.unwrap_or_default().as_list()? {
        ensure!(endings.len() < 128, "locate-file-internal suffix list exceeds limit");
        let entry = entry?;
        let ObjectType::String(value) = entry.untag() else {
            return Err(TypeError::new(Type::String, entry).into());
        };
        ensure!(value.len() <= 65536 && !value.as_bytes().contains(&0), "Invalid locate-file-internal suffix");
        endings.push(String::from(&**value));
    }
    if endings.is_empty() { endings.push(String::new()); }
    let absolute = Path::new(filename).is_absolute();
    let needs_default = !absolute && directories.iter().any(|directory| {
        directory.as_deref().is_none_or(|value| !Path::new(value).is_absolute())
    });
    // Absolute search paths do not depend on default-directory, including an
    // explicitly empty binding. Relative lookup requires a usable base.
    let default = if needs_default {
        let directory = symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?;
        let ObjectType::String(directory) = directory.untag() else {
            return Err(TypeError::new(Type::String, directory).into());
        };
        ensure!(!directory.is_empty(), "Empty default-directory for relative lookup is not implemented");
        ensure!(directory.len() <= 65536 && !directory.as_bytes().contains(&0), "Invalid default-directory");
        let directory = std::path::absolute(Path::new(&**directory))?;
        ensure!(directory.is_absolute(), "default-directory is not absolute");
        directory
    } else { std::path::PathBuf::new() };
    let mut attempts = 0usize;
    for directory in directories {
        let base = match directory {
            Some(directory) => {
                ensure!(directory.len() <= 65536 && !directory.as_bytes().contains(&0), "Invalid locate-file-internal directory");
                default.join(directory)
            }
            None => default.clone(),
        };
        for suffix in &endings {
            attempts += 1;
            ensure!(attempts <= 65536, "locate-file-internal search exceeds limit");
            let mut name = filename.to_owned(); name.push_str(suffix);
            let candidate = if absolute { std::path::PathBuf::from(&name) } else { base.join(&name) };
            let candidate = std::path::absolute(candidate)?;
            ensure!(candidate.is_absolute(), "Search candidate is not absolute");
            // Expand lexically BEFORE checking, so the returned name is the
            // exact name whose readability was tested (including symlink/..).
            let mut normalized = std::path::PathBuf::new();
            for component in candidate.components() {
                match component {
                    Component::CurDir => {},
                    Component::ParentDir => { if normalized.file_name().is_some() { normalized.pop(); } },
                    _ => normalized.push(component.as_os_str()),
                }
            }
            if normalized.is_file() && std::fs::File::open(&normalized).is_ok() {
                return Ok(Some(normalized.to_str().ok_or_else(|| anyhow::anyhow!("Search result is not UTF-8"))?.to_owned()));
            }
        }
        if absolute { break; }
    }
    Ok(None)
}

#[cfg(not(unix))]
fn legacy_expand_file_name(
    name: &str,
    default_directory: Option<&str>,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<String> {
    // TODO: this needs to be tested to ensure it has the same behavior as GNU
    // Emacs. It doesn't do any normalization for one thing.
    if Path::new(name).is_absolute() {
        Ok(name.to_owned())
    } else if let Some(dir) = default_directory {
        let path = Path::new(dir);
        Ok(path.join(name).to_string_lossy().to_string())
    } else {
        let dir = env.vars.get(sym::DEFAULT_DIRECTORY).unwrap();
        match dir.untag(cx) {
            ObjectType::String(dir) => {
                let path = Path::new(dir.as_ref());
                Ok(path.join(name).to_string_lossy().to_string())
            }
            _ => unreachable!("`default-directory' should be a string"),
        }
    }
}

// Lexical local-name expansion: no canonicalize/readlink, account lookup, or IO.
// Handler and tilde expansion remain outside the existing local implementation.
fn normalize_local_path(input: &[u8]) -> Vec<u8> {
    let sep = MAIN_SEPARATOR as u8;
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let at = |n: usize| input.get(n).copied().unwrap_or(0);
        if at(i) != sep { out.push(at(i)); i += 1; }
        else if at(i + 1) == b'.' && (at(i + 2) == sep || at(i + 2) == 0) {
            if out.is_empty() && at(i + 2) == 0 { out.push(sep); }
            i += 2;
        } else if at(i + 1) == b'.' && at(i + 2) == b'.' && !out.is_empty()
            && (at(i + 3) == sep || at(i + 3) == 0) {
            while let Some(byte) = out.pop() { if byte == sep { break; } }
            if out.is_empty() && at(i + 3) == 0 { out.push(sep); }
            i += 3;
        } else if at(i + 1) == sep && (i != 0 || at(i + 2) == sep) { i += 1; }
        else { out.push(sep); i += 1; }
    }
    out
}

// Validate before dispatch or copying. Preserve the original object in the
// condition payload rather than recursively formatting invalid/cyclic values.
fn expand_dispatch_name(value: Object, check_nul: bool, cx: &Context) -> Result<Option<String>> {
    expand_dispatch_name_bounded(value, check_nul, 65536, cx)
}

fn expand_dispatch_name_bounded(value: Object, check_nul: bool, limit: usize, cx: &Context) -> Result<Option<String>> {
    let bytes = match value.untag() {
        ObjectType::String(text) => text.as_bytes(),
        ObjectType::ByteString(bytes) => &**bytes,
        _ => {
            let data = list![sym::STRINGP, value; cx];
            return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into());
        }
    };
    if check_nul && bytes.contains(&0) {
        let data = list![sym::FILENAMEP, value; cx];
        return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into());
    }
    ensure!(bytes.len() <= limit, "Filename handler argument exceeds limit");
    Ok(std::str::from_utf8(bytes).ok().map(str::to_owned))
}

fn call_expand_handler<'ob>(handler: &crate::core::gc::Rto<Object>, name: &crate::core::gc::Rto<Object>, directory: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    use crate::core::{env::{CallFrame, intern}, object::Function};
    use rune_core::macros::root;
    let target: Function = handler.bind(cx).try_into()?;
    root!(target, cx);
    let operation = intern("expand-file-name", cx);
    let mut frame = CallFrame::new(env);
    frame.push_arg(Object::from(operation));
    frame.push_arg(name.bind(cx));
    frame.push_arg(directory.bind(cx));
    root!(result, target.call(&mut frame, None, cx)?, cx);
    drop(frame);
    let result = result.bind(cx);
    if matches!(result.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
        Ok(result)
    } else {
        let data = list!["Invalid handler in `file-name-handler-alist'"; cx];
        Err(crate::eval::EvalError::signal(sym::ERROR.into(), data, cx).into())
    }
}

/// Lisp and qualified access-predicate handler stages; other native callers remain separately scoped.
#[defun(name = "expand-file-name")]
fn expand_file_name_dispatch<'ob>(name: &crate::core::gc::Rto<Object>, directory: Option<&crate::core::gc::Rto<Object>>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    use crate::core::env::intern;
    use rune_core::macros::root;
    #[cfg(not(unix))]
    return expand_file_name_lisp(name.bind(cx), directory.map(|value| value.bind(cx)), env, cx);
    env.initialize_condition_metadata(cx);
    let text = expand_dispatch_name(name.bind(cx), true, cx)?;
    let supplied = directory.map(|value| value.bind(cx)).unwrap_or(NIL.into());
    root!(supplied, cx);
    let handlers = env.variable_value(sym::FILE_NAME_HANDLER_ALIST, cx)?.unwrap_or(NIL.into());
    if handlers.is_nil() {
        return expand_file_name_lisp(name.bind(cx), Some(supplied.bind(cx)), env, cx);
    }
    // The regex engine cannot safely match undecodable raw names. Do not
    // silently skip configured handlers or substitute lossy Unicode text.
    let text = text.ok_or_else(|| anyhow::anyhow!("Raw-byte expansion handlers are unsupported"))?;
    let operation = intern("expand-file-name", cx);
    if let Some(handler) = find_file_name_handler_internal(&text, operation, env, cx) {
        let handler = Object::from(handler); root!(handler, cx);
        return call_expand_handler(handler, name, supplied, env, cx);
    }
    let owner_default = supplied.bind(cx).is_nil();
    let selected = if owner_default {
        symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?
    } else { supplied.bind(cx) };
    let selected = match selected.untag() {
        ObjectType::String(_) | ObjectType::ByteString(_) => selected,
        _ => cx.add("/"),
    };
    root!(selected, cx);
    // GNU validates an owner default's NUL bytes before lookup, but offers an
    // explicit default to its handler first. Do not erase or pre-reject it here.
    let selected_text = expand_dispatch_name(selected.bind(cx), owner_default, cx)?
        .ok_or_else(|| anyhow::anyhow!("Raw-byte expansion default handlers are unsupported"))?;
    // Relative and user-home defaults keep the existing bounded lexical path;
    // recursive GNU default expansion is not implemented or claimed here.
    if selected_text.starts_with('/') {
        let operation = intern("expand-file-name", cx);
        if let Some(handler) = find_file_name_handler_internal(&selected_text, operation, env, cx) {
            let handler = Object::from(handler); root!(handler, cx);
            return call_expand_handler(handler, name, selected, env, cx);
        }
    }
    expand_file_name_lisp(name.bind(cx), Some(supplied.bind(cx)), env, cx)
}

// Bounded lexical core used after filename-expansion handler dispatch.
fn expand_file_name_lisp<'ob>(name: Object, default_directory: Option<Object>, env: &mut Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    #[cfg(not(unix))]
    {
        // Preserve the previous platform implementation outside this Unix fix.
        let name = <&str>::try_from(name)?;
        let directory = default_directory.filter(|value| !value.is_nil()).map(<&str>::try_from).transpose()?;
        return Ok(cx.add(legacy_expand_file_name(name, directory, env, cx)?));
    }

    fn bytes(value: Object, cx: &Context) -> Result<(Vec<u8>, bool)> {
        match value.untag() {
            ObjectType::String(text) => Ok((text.as_bytes().to_vec(), false)),
            ObjectType::ByteString(text) => Ok((text.to_vec(), true)),
            _ => Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), rune_core::macros::list![sym::STRINGP, value; cx], cx).into()),
        }
    }
    env.initialize_condition_metadata(cx);
    let (name, mut raw) = bytes(name, cx)?;
    ensure!(name.len() <= 65536 && !name.contains(&0), "Invalid or excessive filename");
    let sep = MAIN_SEPARATOR as u8;
    let mut joined = if name.first() == Some(&sep) { name.clone() } else {
        let directory = match default_directory.filter(|value| !value.is_nil()) {
            Some(value) => value,
            None => symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?,
        };
        // GNU uses the filesystem root when the chosen default is not a
        // string. No host cwd, account lookup, or guessed buffer state is used.
        let (mut base, base_raw) = match directory.untag() {
            ObjectType::String(_) | ObjectType::ByteString(_) => bytes(directory, cx)?,
            _ => (vec![sep], false),
        };
        ensure!(base.len() <= 65536 && !base.contains(&0), "Invalid or excessive default-directory");
        ensure!(base.first() == Some(&sep), "Relative default-directory is unsupported for local expansion");
        // GNU keeps stored bytes when mixing string kinds: an ASCII operand
        // can adopt the other kind; otherwise retain raw bytes, never decode.
        if raw != base_raw {
            raw = if raw { !name.is_ascii() } else { name.is_ascii() || !base.is_ascii() };
        }
        if base.last() != Some(&sep) { base.push(sep); }
        base.extend_from_slice(&name); base
    };
    ensure!(joined.len() <= 131073, "Expanded filename exceeds limit");
    // Preserve the previous literal-tilde behavior until HOME/user expansion is
    // implemented; normalizing it would silently erase unknown ~user segments.
    if name.first() != Some(&b'~') { joined = normalize_local_path(&joined); }
    if raw { Ok(cx.add(joined)) } else { Ok(cx.add(String::from_utf8(joined)?)) }
}

pub(crate) fn expand_file_name(name: &str, default_directory: Option<&str>, env: &mut Rt<Env>, cx: &Context) -> Result<String> {
    let result = expand_file_name_lisp(cx.add(name), default_directory.map(|value| cx.add(value)), env, cx)?;
    Ok(<&str>::try_from(result)?.to_owned())
}

#[defun]
fn car_less_than_car(a: &Cons, b: &Cons) -> Result<bool> {
    let a: Number = a.car().try_into()?;
    let b: Number = b.car().try_into()?;
    Ok(a.val() < b.val())
}

#[defun]
fn file_accessible_directory_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    #[cfg(not(target_os = "linux"))]
    {
        // Preserve existing behavior outside the scoped Linux implementation.
        let filename = <&str>::try_from(filename.bind(cx))?;
        let path = Path::new(filename);
        return Ok((path.exists() && path.is_dir()).into());
    }
    #[cfg(target_os = "linux")]
    local_file_predicate(filename, LocalFilePredicate::AccessibleDirectory, env, cx)
}

fn file_name_as_directory_lexical(filename: &str) -> String {
    // GNU's local Unix empty name denotes the current directory, not root.
    #[cfg(unix)]
    if filename.is_empty() { return "./".to_owned(); }
    if filename.ends_with(MAIN_SEPARATOR) {
        filename.to_owned()
    } else {
        format!("{filename}{MAIN_SEPARATOR}")
    }
}

/// Convert a file name to directory syntax, preserving handler results as-is.
#[defun]
fn file_name_as_directory<'ob>(
    filename: &crate::core::gc::Rto<Object>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    use crate::core::{env::{CallFrame, intern}, object::Function};
    use rune_core::macros::root;
    #[cfg(not(unix))]
    {
        let text = <&str>::try_from(filename.bind(cx))?;
        return Ok(cx.add(file_name_as_directory_lexical(text)));
    }
    #[cfg(unix)]
    {
        env.initialize_condition_metadata(cx);
        // Preserve the existing uncapped lexical path, including NUL. The
        // shared handler engine's 64-KiB/UTF-8 limits apply only with handlers.
        let handlers = env.variable_value(sym::FILE_NAME_HANDLER_ALIST, cx)?.unwrap_or(NIL.into());
        if !handlers.is_nil() {
            let name = expand_dispatch_name(filename.bind(cx), false, cx)?;
            let name = name.ok_or_else(|| anyhow::anyhow!("Raw-byte file-name-as-directory handlers are unsupported"))?;
            let operation = intern("file-name-as-directory", cx);
            if let Some(handler) = find_file_name_handler_internal(&name, operation, env, cx) {
                let target: Function = Object::from(handler).try_into()?;
                root!(target, cx);
                let mut frame = CallFrame::new(env);
                frame.push_arg(Object::from(operation));
                frame.push_arg(filename.bind(cx));
                root!(result, target.call(&mut frame, None, cx)?, cx);
                drop(frame);
                let result = result.bind(cx);
                if matches!(result.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
                    return Ok(result);
                }
                let data = rune_core::macros::list!["Invalid handler in `file-name-handler-alist'"; cx];
                return Err(crate::eval::EvalError::signal(sym::ERROR.into(), data, cx).into());
            }
        }
        match filename.bind(cx).untag() {
            ObjectType::String(text) => Ok(cx.add(file_name_as_directory_lexical(text))),
            ObjectType::ByteString(bytes) => {
                let mut result = bytes.to_vec();
                if result.is_empty() { result.extend_from_slice(b"./"); }
                else if result.last() != Some(&b'/') { result.push(b'/'); }
                Ok(cx.add(result))
            }
            _ => {
                let data = rune_core::macros::list![sym::STRINGP, filename.bind(cx); cx];
                Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into())
            },
        }
    }
}

#[defun]
fn file_directory_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    #[cfg(not(unix))]
    {
        // Keep existing platform behavior outside this Unix owner-aware change.
        let filename = <&str>::try_from(filename.bind(cx))?;
        return Ok((filename.is_empty() || Path::new(filename).is_dir()).into());
    }
    #[cfg(unix)]
    local_file_predicate(filename, LocalFilePredicate::Directory, env, cx)
}

/// Local lexical directory conversion, without handler dispatch.
fn directory_file_name_lexical(dirname: &str) -> &str {
    #[cfg(unix)]
    {
        // GNU fileio.c directory_file_name preserves exactly "//", but strips
        // every trailing slash otherwise, leaving at least one byte. This is
        // lexical: do not normalize dot components or touch the filesystem.
        let bytes = dirname.as_bytes();
        let mut end = bytes.len();
        if !(end == 2 && bytes[0] == b'/') {
            while end > 1 && bytes[end - 1] == b'/' { end -= 1; }
        }
        &dirname[..end]
    }
    #[cfg(not(unix))]
    {
        let path = Path::new(dirname);
        let mut path_components = path.components();
        if path_components.clone().next().is_none() { return ""; }
        if path_components.all(|c| c == Component::RootDir || c == Component::Normal("".as_ref())) {
            return "/";
        }
        dirname.strip_suffix(MAIN_SEPARATOR).unwrap_or(dirname)
    }
}

/// Lexical directory-name conversion with rooted filename-handler dispatch.
#[defun]
fn directory_file_name<'ob>(
    directory: &crate::core::gc::Rto<Object>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    directory_file_name_dispatch(directory, 65536, env, cx)
}

// Predicates already admit expanded paths up to 131073 bytes. Keep that
// existing budget while the public primitive retains its 64-KiB input limit.
fn directory_file_name_dispatch<'ob>(
    directory: &crate::core::gc::Rto<Object>,
    limit: usize,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    use crate::core::{env::{CallFrame, intern}, object::Function};
    use rune_core::macros::root;
    #[cfg(not(unix))]
    {
        let text = <&str>::try_from(directory.bind(cx))?;
        return Ok(cx.add(directory_file_name_lexical(text)));
    }
    #[cfg(unix)]
    {
        env.initialize_condition_metadata(cx);
        // Unlike filesystem access, lexical conversion does not reject NUL.
        // The shared handler engine still cannot represent undecodable raw bytes.
        let name = expand_dispatch_name_bounded(directory.bind(cx), false, limit, cx)?;
        let handlers = env.variable_value(sym::FILE_NAME_HANDLER_ALIST, cx)?.unwrap_or(NIL.into());
        if !handlers.is_nil() {
            let name = name.ok_or_else(|| anyhow::anyhow!("Raw-byte directory-file-name handlers are unsupported"))?;
            let operation = intern("directory-file-name", cx);
            if let Some(handler) = find_file_name_handler_internal(&name, operation, env, cx) {
                let target: Function = Object::from(handler).try_into()?;
                root!(target, cx);
                let mut frame = CallFrame::new(env);
                frame.push_arg(Object::from(operation));
                frame.push_arg(directory.bind(cx));
                root!(result, target.call(&mut frame, None, cx)?, cx);
                drop(frame);
                let result = result.bind(cx);
                if matches!(result.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
                    return Ok(result);
                }
                let data = rune_core::macros::list!["Invalid handler in `file-name-handler-alist'"; cx];
                return Err(crate::eval::EvalError::signal(sym::ERROR.into(), data, cx).into());
            }
        }
        match directory.bind(cx).untag() {
            ObjectType::String(text) => Ok(cx.add(directory_file_name_lexical(text))),
            ObjectType::ByteString(bytes) => {
                let mut end = bytes.len();
                if !(end == 2 && bytes[0] == b'/') {
                    while end > 1 && bytes[end - 1] == b'/' { end -= 1; }
                }
                Ok(cx.add(bytes[..end].to_vec()))
            }
            _ => unreachable!("directory argument was validated before dispatch"),
        }
    }
}

/// Returns true if the path is absolute
#[defun]
fn file_name_absolute_p(filename: &str) -> bool {
    let path = Path::new(filename);
    // TODO: GNU Emacs has special handling for ~user directories, where the user exists.
    //   so as per example in the manual, ~rms/foo is considered absolute if user `rms` exists
    //   doing this here would require "knowing" the list of valid users and looking for ~path
    //   components.
    path.is_absolute()
}

/// Local filename syntax only: split at the last native separator without
/// Path component normalization. In particular, retain . and .. for file-truename.
/// Existing file-name-handler and DOS drive-syntax support is not extended here.
fn filename_component<'ob>(filename: Object, directory: bool, env: &mut Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    fn split(bytes: &[u8]) -> Option<usize> {
        bytes.iter().rposition(|&byte| byte == MAIN_SEPARATOR as u8).map(|index| index + 1)
    }
    match filename.untag() {
        ObjectType::String(text) => {
            let end = split(text.as_bytes());
            if directory {
                Ok(end.map_or(NIL.into(), |end| cx.add(&text[..end])))
            } else {
                Ok(cx.add(&text[end.unwrap_or(0)..]))
            }
        }
        ObjectType::ByteString(bytes) => {
            let end = split(&bytes);
            if directory {
                Ok(end.map_or(NIL.into(), |end| cx.add(bytes[..end].to_vec())))
            } else {
                Ok(cx.add(bytes[end.unwrap_or(0)..].to_vec()))
            }
        }
        _ => {
            env.initialize_condition_metadata(cx);
            let data = rune_core::macros::list![sym::STRINGP, filename; cx];
            Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into())
        },
    }
}

/// Return the lexical directory prefix, including its final separator, or nil.
#[defun]
fn file_name_directory<'ob>(filename: Object, env: &mut Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    filename_component(filename, true, env, cx)
}

/// Return everything after the final separator, retaining dot components.
#[defun]
fn file_name_nondirectory<'ob>(filename: Object, env: &mut Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    filename_component(filename, false, env, cx)
}

/// Return non-nil if NAME ends with a directory separator character.
#[defun]
fn directory_name_p(name: Object, env: &mut Rt<Env>, cx: &Context) -> Result<bool> {
    // GNU checks the last stored byte, not filesystem state or decoded text.
    // Empty names are not directory names; both Lisp string kinds are valid.
    match name.untag() {
        ObjectType::String(text) => Ok(text.as_bytes().last() == Some(&(MAIN_SEPARATOR as u8))),
        ObjectType::ByteString(bytes) => Ok(bytes.last() == Some(&(MAIN_SEPARATOR as u8))),
        _ => {
            env.initialize_condition_metadata(cx);
            let data = list![sym::STRINGP, name; cx];
            Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into())
        }
    }
}

defvar!(FILE_NAME_HANDLER_ALIST);
defvar!(INHIBIT_FILE_NAME_HANDLERS);
defvar!(INHIBIT_FILE_NAME_OPERATION);

/// Return FILENAME's handler function for OPERATION, if it has one.
/// Otherwise, return nil.
/// A file name is handled if one of the regular expressions in
/// `file-name-handler-alist' matches it.
///
/// If OPERATION equals `inhibit-file-name-operation', then ignore
/// any handlers that are members of `inhibit-file-name-handlers',
/// but still do run any other handlers.  This lets handlers
/// use the standard functions without calling themselves recursively.
#[defun]
fn find_file_name_handler<'ob>(
    filename: &str,
    operation: Symbol,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Symbol<'ob> {
    find_file_name_handler_internal(filename, operation, env, cx).unwrap_or(NIL)
}

fn find_file_name_handler_internal<'ob>(
    filename: &str,
    operation: Symbol,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Option<Symbol<'ob>> {
    let file_name_handler_alist = symbol_value(FILE_NAME_HANDLER_ALIST, env, cx).ok()?.as_list().ok()?;
    let inhibit_file_name_handlers =
        symbol_value(INHIBIT_FILE_NAME_HANDLERS, env, cx).ok().and_then(|sym| sym.as_list().ok());

    let inhibited_operation = symbol_value(sym::INHIBIT_FILE_NAME_OPERATION, env, cx).ok();
    file_name_handler_alist
        .filter_map(|elem| {
            let cons = elem.ok()?.cons()?;
            let regexp = cons.car().string()?;
            let re = Regex::new(&lisp_regex_to_rust(regexp))
                .unwrap_or_else(|err| panic!("Invalid regexp '{regexp}': {err}"));
            if re.is_match(filename).ok()? {
                let handler = cons.cdr().symbol()?;
                if inhibited_operation != Some(operation.into())
                    || !inhibit_file_name_handlers
                        .clone()
                        .is_some_and(|mut ifnh| ifnh.any(|h| h.is_ok_and(|h| h == handler)))
                {
                    return Some(handler);
                }
            }
            maybe_quit();
            None
        })
        .next()
}

#[defun]
fn file_symlink_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    // Mutable-context defuns receive rooted arguments. Copy before any callback.
    let filename = <&str>::try_from(filename.bind(cx))?.to_owned();
    use crate::core::{env::{CallFrame, intern}, object::Function};
    use rune_core::macros::root;
    // Use the current owner/buffer binding. Unsupported relative bases fail
    // closed rather than silently resolving against the host cwd.
    let expanded = if Path::new(&filename).is_absolute() {
        filename.to_owned()
    } else {
        let directory = symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?;
        let ObjectType::String(directory) = directory.untag() else {
            return Err(TypeError::new(Type::String, directory).into());
        };
        ensure!(Path::new(&**directory).is_absolute(), "Relative default-directory is unsupported for file-symlink-p");
        expand_file_name(&filename, Some(&directory), env, cx)?
    };
    let operation = intern("file-symlink-p", cx);
    if let Some(handler) = find_file_name_handler_internal(&expanded, operation, env, cx) {
        let target: Function = Object::from(handler).try_into()?;
        root!(target, cx);
        let mut frame = CallFrame::new(env);
        frame.push_arg(Object::from(operation));
        frame.push_arg(cx.add(expanded));
        return target.call(&mut frame, None, cx).map_err(Into::into);
    }
    // read_link observes the link itself, including dangling links. Never
    // canonicalize or read the target; OS lookup failures are nil, as in GNU.
    let Ok(target) = std::fs::read_link(expanded) else { return Ok(NIL.into()); };
    match target.to_str() {
        Some(text) => Ok(cx.add(text)),
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                Ok(cx.add(target.as_os_str().as_bytes().to_vec()))
            }
            #[cfg(not(unix))]
            { bail!("Symlink target cannot be represented without loss") }
        }
    }
}


/// Local existence with owner-aware expansion and the existing handler protocol.
/// Metadata follows symlinks; lookup errors are nil. Expansion/handler coding
/// limitations are explicit, rather than a host-cwd or lossy-byte fallback.
#[defun]
fn file_exists_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    local_file_predicate(filename, LocalFilePredicate::Exists, env, cx)
}

/// Linux effective-credential readability, sharing expansion and handler rules.
#[defun]
fn file_readable_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    local_file_predicate(filename, LocalFilePredicate::Readable, env, cx)
}

/// Advisory Linux write-or-create access, without writing or creating a file.
#[defun]
fn file_writable_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    local_file_predicate(filename, LocalFilePredicate::Writable, env, cx)
}

/// Regular local file classification; follows symlinks without opening content.
#[defun]
fn file_regular_p<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    local_file_predicate(filename, LocalFilePredicate::Regular, env, cx)
}

#[derive(Clone, Copy)]
enum LocalFilePredicate { Exists, Directory, Readable, AccessibleDirectory, Writable, Regular, Executable }

pub(crate) fn executable_file_predicate<'ob>(filename: &crate::core::gc::Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    local_file_predicate(filename, LocalFilePredicate::Executable, env, cx)
}

fn local_file_predicate<'ob>(filename: &crate::core::gc::Rto<Object>, predicate: LocalFilePredicate, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    use crate::core::{env::{CallFrame, intern}, object::Function};
    use rune_core::macros::root;
    root!(expanded, expand_file_name_dispatch(filename, None, env, cx)?, cx);
    #[cfg(unix)]
    let expanded = {
        let storage = match expanded.bind(cx).untag() {
            ObjectType::String(text) => text.as_bytes(),
            ObjectType::ByteString(bytes) => &**bytes,
            _ => unreachable!("filename expansion returned a non-string"),
        };
        // Preserve the local-path guard before invoking another handler.
        ensure!(storage.len() <= 131073 && !storage.contains(&0), "Invalid or excessive expanded filename");
        if matches!(predicate, LocalFilePredicate::Directory | LocalFilePredicate::Regular)
            && storage.len() > 1 && storage.last() == Some(&b'/')
        {
            directory_file_name_dispatch(expanded, 131073, env, cx)?
        } else {
            expanded.bind(cx)
        }
    };
    #[cfg(not(unix))]
    let expanded = expanded.bind(cx);
    let (storage, raw): (&[u8], bool) = match expanded.untag() {
        ObjectType::String(text) => (text.as_bytes(), false),
        ObjectType::ByteString(bytes) => (&**bytes, true),
        _ => unreachable!("filename expansion returned a non-string"),
    };
    // A handler can return an arbitrary-sized string. Apply the same bounded
    // local-path contract before copying, without truncating embedded NUL.
    ensure!(storage.len() <= 131073 && !storage.contains(&0), "Invalid or excessive expanded filename");
    if matches!(predicate, LocalFilePredicate::Executable) {
        ensure!(storage.len() <= 65536, "File access path exceeds 64 KiB limit");
    }
    let bytes = storage.to_vec();
    let operation_name = match predicate {
        LocalFilePredicate::Exists => "file-exists-p",
        LocalFilePredicate::Directory => "file-directory-p",
        LocalFilePredicate::Readable => "file-readable-p",
        LocalFilePredicate::AccessibleDirectory => "file-accessible-directory-p",
        LocalFilePredicate::Writable => "file-writable-p",
        LocalFilePredicate::Regular => "file-regular-p",
        LocalFilePredicate::Executable => "file-executable-p",
    };
    // Directory/Regular conversion results are used as-is, without re-expansion
    // or repeated suffix conversion. The bounds/NUL guard above applies again.
    // The existing regex handler engine accepts UTF-8 only. Never silently
    // bypass configured handlers for a filename that it cannot represent.
    let handler_name = std::str::from_utf8(&bytes).ok();
    if handler_name.is_none() {
        ensure!(env.variable_value(sym::FILE_NAME_HANDLER_ALIST, cx)?.unwrap_or(NIL.into()).is_nil(),
            "Raw-byte filename handlers are unsupported for {}", operation_name);
    }
    let operation = intern(operation_name, cx);
    if let Some(handler) = handler_name.and_then(|name| find_file_name_handler_internal(name, operation, env, cx)) {
        let target: Function = Object::from(handler).try_into()?;
        root!(target, cx);
        let name = if raw { cx.add(bytes) } else { cx.add(String::from_utf8(bytes)?) };
        let mut frame = CallFrame::new(env);
        frame.push_arg(Object::from(operation));
        frame.push_arg(name);
        return target.call(&mut frame, None, cx).map_err(Into::into);
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
    };
    #[cfg(not(unix))]
    let path = std::path::PathBuf::from(String::from_utf8(bytes)?);
    let result = match predicate {
        LocalFilePredicate::Exists => std::fs::metadata(&path).is_ok(),
        LocalFilePredicate::Directory => std::fs::metadata(&path).is_ok_and(|value| value.is_dir()),
        LocalFilePredicate::Readable => effective_readable_access(&path)?,
        LocalFilePredicate::AccessibleDirectory => effective_directory_access(&path)?,
        LocalFilePredicate::Writable => effective_writable_access(&path)?,
        LocalFilePredicate::Regular => std::fs::metadata(&path).is_ok_and(|value| value.is_file()),
        LocalFilePredicate::Executable => crate::file_access::executable_access(&path)?,
    };
    Ok(result.into())
}

fn effective_readable_access(path: &Path) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        // CString rejects NUL, even if a future caller bypasses expansion's
        // validation. No truncation, content read, or permission-bit inference.
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: the live CString is NUL-terminated; faccessat does not retain
        // its pointer. AT_EACCESS selects effective, not real, credentials.
        let result = unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::R_OK, libc::AT_EACCESS) };
        // GNU returns nil for inaccessible, missing, and other lookup failures;
        // errno is neither exposed nor used as an authorization guarantee.
        Ok(result == 0)
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = path; bail!("Effective readable access is not implemented on this platform") }
}

// Match GNU's POSIX directory-search probe without separate metadata/access
// calls: appending /./ requires traversing the directory, not reading its list.
fn effective_directory_access(path: &Path) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let mut bytes = path.as_os_str().as_bytes().to_vec();
        if !bytes.is_empty() {
            if bytes.last() != Some(&b'/') { bytes.push(b'/'); }
            bytes.extend_from_slice(b"./");
        }
        // Preserve special root // and reject NUL rather than truncating.
        let name = CString::new(bytes)?;
        // SAFETY: the owned CString remains alive during the non-retaining
        // syscall. Effective credentials match GNU file_access_p.
        let result = unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::F_OK, libc::AT_EACCESS) };
        Ok(result == 0)
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = path; bail!("Effective directory access is not implemented on this platform") }
}

fn effective_writable_access(path: &Path) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let bytes = path.as_os_str().as_bytes();
        ensure!(bytes.first() == Some(&b'/'), "Writable access requires an expanded absolute path");
        let name = CString::new(bytes)?;
        // SAFETY: the CString lives through the non-retaining syscall; this is
        // an effective-credential access query, not a write or creation attempt.
        let result = unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::W_OK, libc::AT_EACCESS) };
        if result == 0 { return Ok(true); }
        // Capture errno immediately. Permission failures, ENOTDIR, and all
        // errors other than ENOENT must not become a parent-access success.
        let error = std::io::Error::last_os_error().raw_os_error();
        if error != Some(libc::ENOENT) { return Ok(false); }
        // GNU uses lexical file-name-directory: retain its final separator.
        // A nonexistent name ending in / therefore probes that same directory,
        // not its existing parent. Parent symlinks are resolved by the OS.
        let end = bytes.iter().rposition(|&byte| byte == b'/').expect("absolute path has slash") + 1;
        let parent = CString::new(&bytes[..end])?;
        // SAFETY: owned NUL-terminated parent survives the syscall. Both write
        // and search access are required to create a name in this directory.
        let result = unsafe { libc::faccessat(libc::AT_FDCWD, parent.as_ptr(), libc::W_OK | libc::X_OK, libc::AT_EACCESS) };
        Ok(result == 0)
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = path; bail!("Effective writable access is not implemented on this platform") }
}

#[defun]
fn file_name_case_insensitive_p(filename: &str) -> bool {
    if !Path::new(filename).exists() {
        return false;
    }
    case_insensitive(filename)
}

#[cfg(target_os = "macos")]
fn case_insensitive(filename: &str) -> bool {
    // https://github.com/phracker/MacOSX-SDKs/blob/master/MacOSX10.1.5.sdk/usr/include/sys/unistd.h#L127
    const _PC_CASE_SENSITIVE: libc::c_int = 11;
    let result = unsafe {
        let filename = std::ffi::CString::new(filename).unwrap();
        libc::pathconf(filename.as_ptr(), _PC_CASE_SENSITIVE)
    };
    if result == -1 {
        panic!(
            "file-name-case-insensitive-p pathconf failed: {}",
            std::io::Error::last_os_error()
        )
    }
    result == 0
}

#[cfg(windows)]
fn case_insensitive(filename: &str) -> bool {
    // https://learn.microsoft.com/en-us/windows/wsl/case-sensitivity#inspect-current-case-sensitivity
    let output = std::process::Command::new("fsutil.exe")
        .arg("file")
        .arg("queryCaseSensitiveInfo")
        .arg(filename)
        .output()
        .unwrap()
        .stdout;
    std::str::from_utf8(&output).unwrap().contains("disabled")
}

#[cfg(target_os = "linux")]
fn case_insensitive(_filename: &str) -> bool {
    false
}

#[test]
#[cfg(not(miri))]
fn test_case_sensative_call() {
    let _ = file_name_case_insensitive_p("/");
}

const INSERT_FILE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Bounded strict-UTF8 import. This does not visit, replace or detect coding.
#[defun]
#[expect(clippy::too_many_arguments)]
fn insert_file_contents<'ob>(
    filename: &str, visit: OptionalFlag, begin: Option<i64>, end: Option<i64>,
    replace: OptionalFlag, env: &Rt<Env>, cx: &'ob Context,
) -> Result<Object<'ob>> {
    use std::io::Read;
    use rune_core::macros::list;
    ensure!(visit.is_none(), "insert-file-contents VISIT is not implemented");
    ensure!(begin.is_none() && end.is_none(), "insert-file-contents byte slicing is not implemented");
    ensure!(replace.is_none(), "insert-file-contents REPLACE is not implemented");
    let path = if Path::new(filename).is_absolute() {
        Path::new(filename).to_owned()
    } else {
        let directory = symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?;
        let ObjectType::String(directory) = directory.untag() else {
            return Err(TypeError::new(Type::String, directory).into());
        };
        Path::new(&**directory).join(filename)
    };
    let path = std::path::absolute(path)?;
    ensure!(path.is_absolute(), "Import path could not be made absolute");
    let returned_name = path.to_str().ok_or_else(|| anyhow::anyhow!("Non-UTF8 import paths cannot be represented by Lisp strings"))?.to_owned();
    let before = env.buffer_read(cx)?;
    let metadata = std::fs::metadata(&path)?;
    ensure!(metadata.is_file(), "Import source must be a regular file");
    ensure!(metadata.len() <= INSERT_FILE_MAX_BYTES as u64, "Import exceeds 16MiB byte limit");
    let file = std::fs::File::open(&path)?;
    ensure!(file.metadata()?.is_file(), "Import source is no longer a regular file");
    let mut bytes = Vec::new();
    file.take(INSERT_FILE_MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= INSERT_FILE_MAX_BYTES, "Import exceeds 16MiB byte limit");
    let text = String::from_utf8(bytes)?;
    let count = text.chars().count();
    let result = list![returned_name, count; cx];
    if !text.is_empty() {
        env.editor_gateway().insert_preserving_point(before.handle, before.epoch, before.point, &text, cx)?;
    }
    Ok(result)
}

// Deliberately bounded, UTF-8 visited-only save surface. No interactive chooser.
const SAVE_BUFFER_MAX_BYTES: usize = 16 * 1024 * 1024;
#[defun]
fn save_buffer(prefix: OptionalFlag, env: &Rt<Env>, cx: &Context) -> Result<()> {
    ensure!(prefix.is_none(), "save-buffer prefix/backup modes are not implemented");
    save_current_visited_buffer(SAVE_BUFFER_MAX_BYTES, env, cx)
}
fn save_current_visited_buffer(max_bytes: usize, env: &Rt<Env>, cx: &Context) -> Result<()> {
    let gateway = env.editor_gateway();
    let read = env.buffer_read(cx)?;
    let path = read.file.path().ok_or_else(|| anyhow::anyhow!("Buffer has no visited file"))?;
    // Existing clean files are not overwritten; a missing visited file is
    // recreated, as GNU does. This is not external-modification detection.
    if !gateway.modified(read.handle, cx)? && path.try_exists()? { return Ok(()); }
    let pending = gateway.prepare_visited_save(read.handle, read.epoch, max_bytes, cx)?;
    // This call holds no gateway borrow. Only successful checkpoint completion
    // marks saved; an error after disk success must never trigger I/O replay.
    let written = pending.write(crate::editor_document::SaveMode::ReplaceExisting)?;
    gateway.finish_save(written, cx)
        .map_err(|error| error.context("File snapshot written, but save checkpoint rejected; do not replay the write"))?;
    Ok(())
}

defsym!(EXCL);
#[defun(name = "write-region")]
#[expect(clippy::too_many_arguments)]
fn write_region_lisp(
    start: Object, end: Object, filename: &str, append: Option<Object>,
    visit: OptionalFlag, lockname: OptionalFlag, mustbenew: Option<Object>,
    env: &Rt<Env>, cx: &Context,
) -> Result<()> {
    if start.is_nil() {
        let end = env.buffer_read(cx)?.document.text().len_chars().checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Buffer position overflow"))?;
        return write_region(1, i64::try_from(end)?, filename, append, visit, lockname, mustbenew, env, cx);
    }
    match start.untag() {
        ObjectType::String(text) => write_region_to(filename, append, visit, lockname, mustbenew, |file| {
            std::io::Write::write_all(file, text.as_bytes())?;
            Ok(())
        }),
        ObjectType::ByteString(bytes) => write_region_to(filename, append, visit, lockname, mustbenew, |file| {
            std::io::Write::write_all(file, bytes)?;
            Ok(())
        }),
        ObjectType::Int(start) => {
            let ObjectType::Int(end) = end.untag() else { bail!("END must be a fixnum buffer position") };
            write_region(start, end, filename, append, visit, lockname, mustbenew, env, cx)
        }
        _ => bail!("START must be nil, a string, or a fixnum buffer position"),
    }
}

#[expect(clippy::too_many_arguments)]
fn write_region(
    start: i64, end: i64, filename: &str, append: Option<Object>,
    visit: OptionalFlag, lockname: OptionalFlag, mustbenew: Option<Object>,
    env: &Rt<Env>, cx: &Context,
) -> Result<()> {
    use std::io::Write;
    // Resolve every scalar range before opening/truncating the destination.
    let start = usize::try_from(start)?;
    let end = usize::try_from(end)?;
    let (start, end) = (start.min(end), start.max(end));
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    ensure!(start > 0 && end <= text.len_chars() + 1, "Buffer range out of bounds");
    let bytes = text.char_to_byte(rune_text::CharPos(start - 1))?
        ..text.char_to_byte(rune_text::CharPos(end - 1))?;
    let chunks = text.chunks(bytes)?;
    write_region_to(filename, append, visit, lockname, mustbenew, |file| {
        for chunk in chunks { file.write_all(chunk.as_bytes())?; }
        Ok(())
    })
}

fn write_region_to(
    filename: &str, append: Option<Object>, visit: OptionalFlag,
    lockname: OptionalFlag, mustbenew: Option<Object>,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    enum Destination { Truncate, Append, Offset(u64) }
    let destination = match append {
        None => Destination::Truncate,
        Some(value) if value.is_nil() => Destination::Truncate,
        Some(value) => match value.untag() {
            ObjectType::Int(offset) => {
                ensure!(offset >= 0, "Negative file offset");
                Destination::Offset(offset as u64)
            }
            ObjectType::BigInt(_) => bail!("File offset outside supported fixnum range"),
            ObjectType::Float(_) => bail!("Expected integer file offset"),
            _ => Destination::Append,
        },
    };
    ensure!(visit.is_none(), "visit not implemented");
    ensure!(lockname.is_none(), "lockname not implemented");
    let exclusive = match mustbenew {
        None => false,
        Some(value) if value.is_nil() => false,
        Some(value) if value == Object::from(sym::EXCL) => true,
        _ => bail!("Confirmation-requiring mustbenew modes are not implemented"),
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .create_new(exclusive)
        .truncate(matches!(destination, Destination::Truncate))
        .append(matches!(destination, Destination::Append))
        .open(filename)?;
    if let Destination::Offset(offset) = destination {
        file.seek(SeekFrom::Start(offset))?;
    }
    write(&mut file)
}

/// Concatenate local UTF-8 components, filtering nil/empty strings as GNU does.
/// No expansion or normalization occurs. Existing raw-byte string support is
/// not extended here; GNU byte/multibyte result-kind rules remain a separate gap.
#[defun]
fn file_name_concat(directory: Object, rest_components: ArgSlice, env: &mut Rt<Env>, cx: &Context) -> Result<String> {
    env.initialize_condition_metadata(cx);
    let mut path = String::new();
    for component in std::iter::once(directory).chain(env.stack.arg_slice(rest_components).iter().map(|value| value.bind(cx))) {
        let text = match component.untag() {
            ObjectType::NIL => continue,
            ObjectType::String(text) => text,
            ObjectType::ByteString(_) => bail!("file-name-concat raw-byte strings are unsupported"),
            _ => {
                let data = list![sym::STRINGP, component; cx];
                return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into());
            }
        };
        if text.is_empty() { continue; }
        // An absolute later component is still concatenated, not a replacement
        // for earlier components. Preserve repeated separators and dot segments.
        if !path.is_empty() && !path.ends_with(MAIN_SEPARATOR) { path.push(MAIN_SEPARATOR); }
        path.push_str(text.as_ref());
    }
    Ok(path)
}

#[defun]
fn unix_sync() {
    sync_filesystems();
}

#[cfg(unix)]
fn sync_filesystems() {
    unsafe { libc::sync() }
}

/// Windows has no system wide equivalent of `sync(2)`; buffers can only be
/// flushed per open handle, which we have no way to enumerate here.
#[cfg(not(unix))]
fn sync_filesystems() {}

// TODO: file-relative-name -- requires knowing the current buffer's default directory
// TODO: file-name-sans-versions
// TODO: find-file-name-handler: https://www.gnu.org/software/emacs/manual/html_node/elisp/Magic-File-Names.html
//   required by file-name-extension  & file-name-sans-extension library & file-relative-name functions (among others)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        assert_elprop,
        core::{env::sym, gc::RootSet},
        data::set,
        sym::INHIBIT_FILE_NAME_HANDLERS,
    };
    use rune_core::macros::{list, root};

    #[test]
    fn test_find_file_name_handler_matching() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, init(Env::new(cx).unwrap()), cx);

        let handler = Symbol::new_uninterned("matching-handler", cx);
        let not_matching_handler = Symbol::new_uninterned("not-matching-handler", cx);
        let file_name_handler_alist = list![
            Cons::new(r"test\.txt", handler, cx),
            Cons::new("NEVER-MATCH", not_matching_handler, cx);
            cx
        ];
        set(FILE_NAME_HANDLER_ALIST, file_name_handler_alist, env, cx).unwrap();

        let test_txt_handler = find_file_name_handler("test.txt", NIL, env, cx);
        assert_eq!(test_txt_handler, handler);
        let not_matching_handler = find_file_name_handler("not-matching.el", NIL, env, cx);
        assert_eq!(not_matching_handler, NIL);
    }

    #[test]
    fn test_find_file_name_handler_inhibit_handlers() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, init(Env::new(cx).unwrap()), cx);

        let handler = Symbol::new_uninterned("matching-handler", cx);
        let inhibit_handler = Symbol::new_uninterned("inhibit-handler", cx);
        let file_name_handler_alist = list![
            Cons::new(r"test\.txt", inhibit_handler, cx),
            Cons::new(r"test\.txt", handler, cx);
            cx
        ];
        set(FILE_NAME_HANDLER_ALIST, file_name_handler_alist, env, cx).unwrap();
        set(INHIBIT_FILE_NAME_HANDLERS, list![inhibit_handler; cx], env, cx).unwrap();

        let operation = Symbol::new_uninterned("test-operation", cx);
        set(sym::INHIBIT_FILE_NAME_OPERATION, operation.into(), env, cx).unwrap();
        let result = find_file_name_handler("test.txt", NIL, env, cx);
        assert_eq!(result, inhibit_handler);

        let result = find_file_name_handler("test.txt", operation, env, cx);
        assert_eq!(result, handler);
    }

    #[test]
    fn test_find_file_name_handler_no_panic() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let handler = Symbol::new_uninterned("handler", cx);
        root!(env, init(Env::new(cx).unwrap()), cx);

        set(FILE_NAME_HANDLER_ALIST, list![Cons::new(NIL, handler, cx); cx], env, cx).unwrap();
        find_file_name_handler("example", NIL, env, cx);

        set(FILE_NAME_HANDLER_ALIST, list![Cons::new(".*", NIL , cx); cx], env, cx).unwrap();
        find_file_name_handler("example", NIL, env, cx);
    }

    #[test]
    #[should_panic(
        expected = "Invalid regexp '\\\\(incorrect-regexp': Parsing error at position 17: Opening parenthesis without closing parenthesis"
    )]
    fn test_find_file_name_handler_panic_on_invalid_regex() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let handler = Symbol::new_uninterned("handler", cx);
        root!(env, init(Env::new(cx).unwrap()), cx);

        set(
            FILE_NAME_HANDLER_ALIST,
            list![Cons::new(r#"\(incorrect-regexp"#, handler, cx); cx],
            env,
            cx,
        )
        .unwrap();
        find_file_name_handler("example", NIL, env, cx);
    }

    #[test]
    #[ignore = "TODO: Handle ~ in expand-file-name"]
    fn test_expand_file_name() {
        assert_elprop![r#"(expand-file-name "~/test.txt" "/")"#];
    }
}

#[cfg(test)]
mod recovered_write_region_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn invalid_windows_cannot_truncate_and_unicode_reversal_is_exact() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rune-owner-write-{}-{unique}", std::process::id()));
        std::fs::create_dir(&dir)?;
        let existing = dir.join("existing");
        let absent = dir.join("absent");
        std::fs::write(&existing, b"must survive")?;
        for (start, end) in [(-1, 1), (0, 1), (1, 2), (2, 1), (1, i64::MAX)] {
            for path in [&existing, &absent] {
                assert!(
                    write_region(
                        start,
                        end,
                        path.to_str().unwrap(),
                        None,
                        None,
                        None,
                        None,
                        env,
                        cx
                    )
                    .is_err()
                )
            }
            assert_eq!(std::fs::read(&existing)?, b"must survive");
            assert!(!absent.exists())
        }
        let gateway = env.editor_gateway();
        let read = env.buffer_read(cx)?;
        gateway.splice(read.handle, read.epoch, 0..0, "aé🙂\0z", cx)?;
        gateway.set_point(read.handle, 2, cx)?;
        let read = env.buffer_read(cx)?;
        gateway.splice(read.handle, read.epoch, 2..2, "λ", cx)?;
        for (start, end) in [(2, 6), (6, 2)] {
            write_region(start, end, existing.to_str().unwrap(), None, None, None, None, env, cx)?;
            assert_eq!(std::fs::read(&existing)?, "éλ🙂\0".as_bytes())
        }
        write_region(1, 1, existing.to_str().unwrap(), None, None, None, None, env, cx)?;
        assert!(std::fs::read(&existing)?.is_empty());
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod streamed_write_append_tests {
    use super::*;
    use crate::core::{gc::RootSet, object::TRUE};
    use rune_core::macros::root;
    #[test]
    fn append_and_byte_offset_preserve_other_bytes_and_buffer_state() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let g = env.editor_gateway();
        let initial = env.buffer_read(cx)?;
        g.splice(initial.handle, initial.epoch, 0..0, "aé🙂", cx)?;
        let before = env.buffer_read(cx)?;
        let dir = std::env::temp_dir().join(format!("rune-append-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let file = dir.join("existing");
        let absent = dir.join("absent");
        std::fs::write(&file, b"0123456789abcdef")?;
        write_region(1, 4, file.to_str().unwrap(), Some(TRUE), None, None, None, env, cx)?;
        assert_eq!(std::fs::read(&file)?, "0123456789abcdefaé🙂".as_bytes());
        std::fs::write(&file, b"0123456789abcdef")?;
        write_region(1, 4, file.to_str().unwrap(), Some(2_i64.into()), None, None, None, env, cx)?;
        assert_eq!(std::fs::read(&file)?, "01aé🙂9abcdef".as_bytes());
        for path in [&file, &absent] {
            assert!(write_region(1, 4, path.to_str().unwrap(), Some((-1_i64).into()), None, None, None, env, cx).is_err());
            assert!(write_region(0, 4, path.to_str().unwrap(), Some(TRUE), None, None, None, env, cx).is_err());
        }
        assert_eq!(std::fs::read(&file)?, "01aé🙂9abcdef".as_bytes());
        assert!(!absent.exists());
        write_region(1, 4, absent.to_str().unwrap(), Some(TRUE), None, None, None, env, cx)?;
        assert_eq!(std::fs::read(&absent)?, "aé🙂".as_bytes());
        let after = env.buffer_read(cx)?;
        assert_eq!(after.epoch, before.epoch);
        assert_eq!(after.point, before.point);
        assert!(g.modified(before.handle, cx)?);
        Ok(())
    }
}

#[cfg(test)]
mod exclusive_write_region_tests {
    use super::*;
    use crate::core::{gc::RootSet, object::TRUE};
    use rune_core::macros::root;
    #[test]
    fn exclusive_creation_never_clobbers_existing_targets() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let g = env.editor_gateway();
        let initial = env.buffer_read(cx)?;
        g.splice(initial.handle, initial.epoch, 0..0, "é🙂", cx)?;
        let dir = std::env::temp_dir().join(format!("rune-exclusive-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let target = dir.join("target");
        let absent = dir.join("absent");
        std::fs::write(&target, b"must survive")?;
        let exclusive = Some(sym::EXCL.into());
        for append in [None, Some(TRUE), Some(0_i64.into())] {
            assert!(write_region(1, 3, target.to_str().unwrap(), append, None, None, exclusive, env, cx).is_err());
            assert_eq!(std::fs::read(&target)?, b"must survive");
        }
        assert!(write_region(1, 1, target.to_str().unwrap(), None, None, None, exclusive, env, cx).is_err());
        assert_eq!(std::fs::read(&target)?, b"must survive");
        assert!(write_region(1, 3, absent.to_str().unwrap(), Some(cx.add(1.5_f64)), None, None, exclusive, env, cx).is_err());
        assert!(write_region(0, 3, absent.to_str().unwrap(), None, None, None, exclusive, env, cx).is_err());
        assert!(write_region(1, 3, absent.to_str().unwrap(), None, None, None, Some(TRUE), env, cx).is_err());
        assert!(!absent.exists());
        write_region(1, 3, absent.to_str().unwrap(), None, None, None, exclusive, env, cx)?;
        assert_eq!(std::fs::read(&absent)?, "é🙂".as_bytes());
        #[cfg(unix)] {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&target, &link)?;
            assert!(write_region(1, 3, link.to_str().unwrap(), None, None, None, exclusive, env, cx).is_err());
            assert_eq!(std::fs::read(&target)?, b"must survive");
            let dangling_target = dir.join("dangling-target");
            let dangling_link = dir.join("dangling-link");
            std::os::unix::fs::symlink(&dangling_target, &dangling_link)?;
            assert!(write_region(1, 3, dangling_link.to_str().unwrap(), None, None, None, exclusive, env, cx).is_err());
            assert!(!dangling_target.exists());
        }
        Ok(())
    }
}

#[cfg(test)]
mod append_float_rejection_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn float_offsets_reject_before_creating_or_modifying_files() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let g = env.editor_gateway();
        let before = env.buffer_read(cx)?;
        g.splice(before.handle, before.epoch, 0..0, "é🙂", cx)?;
        let dir = std::env::temp_dir().join(format!("rune-float-offset-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let existing = dir.join("existing");
        let absent = dir.join("absent");
        std::fs::write(&existing, b"must survive")?;
        for offset in [1.5, 0.0, -0.0, f64::NAN, f64::INFINITY] {
            for path in [&existing, &absent] {
                assert!(write_region(1, 3, path.to_str().unwrap(), Some(cx.add(offset)), None, None, None, env, cx).is_err());
            }
            assert_eq!(std::fs::read(&existing)?, b"must survive");
            assert!(!absent.exists());
        }
        Ok(())
    }
}

#[cfg(test)]
mod visited_save_buffer_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    use rune_editor::{Command, EditorHost};
    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!("rune-lisp-save-{}-{}", std::process::id(), rand::random::<u64>()));
            std::fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }
    impl Drop for Directory { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    #[test]
    fn lisp_save_targets_env_buffer_and_fences_real_save_undo() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        let gateway = env.editor_gateway();
        gateway.rename(env.current_buffer_handle(), "lisp-visited-save", false, cx)?;
        let before = env.buffer_read(cx)?;
        gateway.splice(before.handle, before.epoch, 0..0, "é🙂", cx)?;
        let dir = Directory::new()?;
        let path = dir.0.join("env-file");
        std::fs::write(&path, b"old")?;
        gateway.set_file_association(before.handle, before.file.revision(), Some(&path), cx)?;
        let ui = gateway.scratch(cx)?;
        let ui_before = gateway.read(ui, cx)?;
        gateway.splice(ui, ui_before.epoch, 0..0, "UI", cx)?;
        let ui_path = dir.0.join("ui-file");
        std::fs::write(&ui_path, b"UI sentinel")?;
        gateway.set_file_association(ui, ui_before.file.revision(), Some(&ui_path), cx)?;
        let edited = env.buffer_read(cx)?;
        crate::lread::load_internal("(save-buffer)", cx, env)?;
        assert_eq!(std::fs::read(&path)?, "é🙂".as_bytes());
        assert_eq!(std::fs::read(&ui_path)?, b"UI sentinel");
        assert!(!gateway.modified(before.handle, cx)?);
        assert!(gateway.modified(ui, cx)?);
        assert_eq!(env.buffer_read(cx)?.epoch, edited.epoch);
        let mut native = gateway.host(cx)?;
        let selected = native.snapshot()?.buffers.iter().find(|b| b.name == before.name).unwrap().id;
        native.execute(Command::SwitchBuffer(selected))?;
        native.execute(Command::Undo)?;
        assert!(gateway.modified(before.handle, cx)?);
        native.execute(Command::Redo)?;
        assert!(!gateway.modified(before.handle, cx)?);
        Ok(())
    }
    #[test]
    fn clean_save_preserves_existing_changes_but_recreates_missing_file() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let gateway = env.editor_gateway();
        let before = env.buffer_read(cx)?;
        gateway.splice(before.handle, before.epoch, 0..0, "inside", cx)?;
        let dir = Directory::new()?;
        let path = dir.0.join("file");
        gateway.set_file_association(before.handle, before.file.revision(), Some(&path), cx)?;
        save_buffer(None, env, cx)?;
        std::fs::write(&path, b"external")?;
        save_buffer(None, env, cx)?;
        assert_eq!(std::fs::read(&path)?, b"external");
        assert!(!gateway.modified(before.handle, cx)?);
        std::fs::remove_file(&path)?;
        save_buffer(None, env, cx)?;
        assert_eq!(std::fs::read(&path)?, b"inside");
        assert!(!gateway.modified(before.handle, cx)?);
        Ok(())
    }
    #[test]
    fn unsupported_or_failed_save_preserves_files_and_dirty_state() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let gateway = env.editor_gateway();
        let before = env.buffer_read(cx)?;
        gateway.splice(before.handle, before.epoch, 0..0, "é🙂", cx)?;
        assert!(save_buffer(None, env, cx).is_err());
        let dir = Directory::new()?;
        let path = dir.0.join("existing");
        std::fs::write(&path, b"must survive")?;
        gateway.set_file_association(before.handle, before.file.revision(), Some(&path), cx)?;
        assert!(save_buffer(Some(()), env, cx).is_err());
        assert!(save_current_visited_buffer(1, env, cx).is_err());
        assert_eq!(std::fs::read(&path)?, b"must survive");
        let read = env.buffer_read(cx)?;
        gateway.set_file_association(before.handle, read.file.revision(), Some(&dir.0), cx)?;
        assert!(save_buffer(None, env, cx).is_err());
        assert!(gateway.modified(before.handle, cx)?);
        assert_eq!(env.buffer_read(cx)?.epoch, read.epoch);
        Ok(())
    }
}

#[cfg(test)]
mod write_region_source_form_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!("rune-write-source-{}-{}", std::process::id(), rand::random::<u64>()));
            std::fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }
    impl Drop for Directory { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    #[test]
    fn lisp_nil_and_string_sources_ignore_end_without_mutating_buffer() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        let g = env.editor_gateway();
        let empty = env.buffer_read(cx)?;
        g.splice(empty.handle, empty.epoch, 0..0, "aé🙂z", cx)?;
        g.set_point(empty.handle, 2, cx)?;
        let before = env.buffer_read(cx)?;
        let dir = Directory::new()?;
        let path = dir.0.join("file");
        for (start, end, expected) in [
            ("nil", "nil", "aé🙂z".as_bytes()),
            ("nil", "'ignored", "aé🙂z".as_bytes()),
            ("nil", "1.5", "aé🙂z".as_bytes()),
            (r#""xy""#, "nil", b"xy".as_slice()),
            (r#""λ🙂""#, "'(ignored)", "λ🙂".as_bytes()),
            ("(unibyte-string 0 255 65)", "'ignored", &[0, 255, 65]),
        ] {
            crate::lread::load_internal(&format!("(write-region {start} {end} {:?})", path.to_str().unwrap()), cx, env)?;
            assert_eq!(std::fs::read(&path)?, expected);
        }
        let after = env.buffer_read(cx)?;
        assert_eq!(after.epoch, before.epoch);
        assert_eq!(after.point, before.point);
        assert!(g.modified(before.handle, cx)?);
        Ok(())
    }
    #[test]
    fn invalid_source_windows_and_modes_never_touch_destination() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        let dir = Directory::new()?;
        let existing = dir.0.join("existing");
        let absent = dir.0.join("absent");
        std::fs::write(&existing, b"must survive")?;
        for path in [&existing, &absent] {
            for (start, end, options) in [
                ("1", "nil", ""), ("t", "2", ""), ("1.5", "2", ""),
                ("1", r#""bad""#, ""), ("0", "1", ""), ("1", "99", ""),
                (r#""xy""#, "'ignored", "1.5"),
                (r#""xy""#, "nil", "nil t"),
                (r#""xy""#, "nil", "nil nil nil t"),
            ] {
                assert!(crate::lread::load_internal(&format!("(write-region {start} {end} {:?} {options})", path.to_str().unwrap()), cx, env).is_err());
                assert_eq!(std::fs::read(&existing)?, b"must survive");
                assert!(!absent.exists());
            }
        }
        Ok(())
    }
    #[test]
    fn concurrent_string_source_exclusive_create_has_one_complete_winner() -> Result<()> {
        let dir = Directory::new()?;
        let path = dir.0.join("target");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for payload in ["first-é", "second-🙂"] {
            let path = path.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let roots = RootSet::default();
                let cx = &Context::new(&roots);
                root!(env, init(Env::new_isolated(cx).unwrap()), cx);
                let source = cx.add(payload);
                barrier.wait();
                let won = write_region_lisp(source, NIL.into(), path.to_str().unwrap(), None, None, None, Some(sym::EXCL.into()), env, cx).is_ok();
                (won, payload)
            }));
        }
        let outcomes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let winners: Vec<_> = outcomes.iter().filter(|(won, _)| *won).collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(std::fs::read(&path)?, winners[0].1.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod bounded_insert_file_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    use rune_editor::{Command, EditorHost};
    struct Directory(std::path::PathBuf);
    impl Directory { fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("rune-import-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&path)?; Ok(Self(path))
    }}
    impl Drop for Directory { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    #[test]
    fn actual_lisp_import_counts_scalars_keeps_point_and_is_one_undo() -> Result<()> {
        let roots = RootSet::default(); let cx = &mut Context::new(&roots); sym::init_symbols();
        root!(env, init(Env::new(cx)?), cx);
        let g = env.editor_gateway(); let empty = env.buffer_read(cx)?;
        g.splice(empty.handle, empty.epoch, 0..0, "AB", cx)?;
        g.set_point(empty.handle, 1, cx)?; g.set_modified(empty.handle, false, cx)?;
        let before = env.buffer_read(cx)?;
        let dir = Directory::new()?; let path = dir.0.join("synthetic.org");
        std::fs::write(&path, "aé🙂\n")?;
        crate::lread::load_internal(&format!(r#"(setq imported (insert-file-contents {:?}))
          (if (and (equal (car imported) {:?}) (= (car (cdr imported)) 4)
                   (= (length imported) 2) (= (point) 2)
                   (equal (buffer-substring-no-properties 1 (point-max)) "Aaé🙂\nB"))
              nil (error "wrong import result"))"#, path.to_str().unwrap(), path.to_str().unwrap()), cx, env)?;
        assert_eq!(env.buffer_read(cx)?.epoch, before.epoch + 1);
        assert!(g.modified(before.handle, cx)?); assert_eq!(g.file_name(before.handle, cx)?, None);
        g.host(cx)?.execute(Command::Undo)?;
        assert_eq!(env.buffer_read(cx)?.document.text().to_string(), "AB");
        assert_eq!(std::fs::read(&path)?, "aé🙂\n".as_bytes());
        Ok(())
    }
    #[test]
    fn failed_reads_modes_and_readonly_imports_are_nonmutating() -> Result<()> {
        let roots = RootSet::default(); let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let g = env.editor_gateway(); let before = env.buffer_read(cx)?;
        let dir = Directory::new()?; let valid = dir.0.join("valid"); let invalid = dir.0.join("invalid"); let huge = dir.0.join("huge");
        std::fs::write(&valid, "λ🙂")?; std::fs::write(&invalid, [0xff])?;
        std::fs::File::create(&huge)?.set_len(INSERT_FILE_MAX_BYTES as u64 + 1)?;
        for path in [&invalid, &huge, &dir.0, &dir.0.join("missing")] {
            assert!(insert_file_contents(path.to_str().unwrap(), None, None, None, None, env, cx).is_err());
        }
        for (visit, begin, end, replace) in [(Some(()), None, None, None), (None, Some(0), None, None), (None, None, Some(1), None), (None, None, None, Some(()))] {
            assert!(insert_file_contents(valid.to_str().unwrap(), visit, begin, end, replace, env, cx).is_err());
        }
        g.set_read_only(before.handle, true, cx)?;
        assert!(insert_file_contents(valid.to_str().unwrap(), None, None, None, None, env, cx).is_err());
        assert_eq!(env.buffer_read(cx)?.epoch, before.epoch); assert_eq!(env.buffer_read(cx)?.point, before.point);
        assert_eq!(env.buffer_read(cx)?.document.text().to_string(), "");
        Ok(())
    }
    #[test]
    fn relative_names_use_default_directory_and_empty_file_is_noop() -> Result<()> {
        let roots = RootSet::default(); let cx = &mut Context::new(&roots); sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        let dir = Directory::new()?; let path = dir.0.join("relative.org"); std::fs::write(&path, "")?;
        env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.0.to_str().unwrap()))?;
        let before = env.buffer_read(cx)?;
        crate::lread::load_internal(&format!(r#"(setq imported (insert-file-contents "relative.org"))
          (if (and (equal (car imported) {:?}) (= (car (cdr imported)) 0) (= (point) 1))
              nil (error "wrong empty import"))"#, path.to_str().unwrap()), cx, env)?;
        assert_eq!(env.buffer_read(cx)?.epoch, before.epoch);
        assert!(!env.editor_gateway().modified(before.handle, cx)?);
        Ok(())
    }
}

#[cfg(test)]
mod locate_file_internal_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::{list, root, rebind};
    #[test]
    fn actual_lisp_search_order_suffix_order_and_default_directory() -> Result<()> {
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(dir.join("first"))?;
        std::fs::create_dir_all(dir.join("second"))?;
        let result = (|| -> Result<()> {
            std::fs::write(dir.join("first/pkg.extra.el"), "first")?;
            std::fs::write(dir.join("first/pkg.extra"), "bare")?;
            std::fs::write(dir.join("second/pkg.extra.el"), "second")?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            let source = r#"(locate-file-internal "pkg.extra" '("first" "second") '(".el" ""))"#;
            let form = crate::reader::read(source, cx)?.0; root!(form, cx);
            let actual = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
            assert_eq!(<&str>::try_from(actual)?, dir.join("first/pkg.extra.el").to_str().unwrap());
            assert_eq!(locate_file_internal("first/pkg.extra", NIL.into(), None, None, env, cx)?, Some(dir.join("first/pkg.extra").to_str().unwrap().to_owned()));
            assert!(locate_file_internal("first", NIL.into(), None, None, env, cx)?.is_none());
            assert!(locate_file_internal("missing", NIL.into(), None, None, env, cx)?.is_none());
            #[cfg(unix)] {
                std::fs::create_dir_all(dir.join("a"))?;
                std::fs::create_dir_all(dir.join("b/subdir"))?;
                std::fs::write(dir.join("b/target"), "physical-only")?;
                std::os::unix::fs::symlink(dir.join("b/subdir"), dir.join("a/link"))?;
                assert!(locate_file_internal("a/link/../target", NIL.into(), None, None, env, cx)?.is_none());
                std::fs::write(dir.join("a/target"), "lexical")?;
                assert_eq!(locate_file_internal("a/link/../target", NIL.into(), None, None, env, cx)?, Some(dir.join("a/target").to_str().unwrap().to_owned()));
            }
            assert!(locate_file_internal("missing", list!["x".repeat(65537); cx], None, None, env, cx).is_err());
            assert!(locate_file_internal("pkg", list![42i64; cx], None, None, env, cx).is_err());
            assert!(locate_file_internal("pkg", NIL.into(), Some(list![42i64; cx]), None, env, cx).is_err());
            assert!(locate_file_internal("pkg", NIL.into(), None, Some(()), env, cx).is_err());
            let circular = Cons::new(".", NIL, cx);
            circular.set_cdr(circular.into())?;
            assert!(locate_file_internal("missing", circular.into(), None, None, env, cx).is_err());
            let mut too_many = Object::from(NIL);
            for _ in 0..129 { too_many = Cons::new(".el", too_many, cx).into(); }
            assert!(locate_file_internal("pkg", NIL.into(), Some(too_many), None, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}

#[cfg(test)]
mod startup_directory_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::{list, root};
    #[test]
    fn startup_directory_is_real_absolute_cwd_and_absolute_lookup_ignores_empty_override() -> Result<()> {
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        cx.garbage_collect(true);
        let value = symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?;
        let directory = <&str>::try_from(value)?;
        assert!(Path::new(directory).is_absolute());
        assert!(directory.ends_with(std::path::MAIN_SEPARATOR));
        assert_eq!(Path::new(directory), std::env::current_dir()?.as_path());
        let dir = std::env::temp_dir().join(format!("rune-empty-default-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?; std::fs::write(dir.join("target.el"), "test")?;
        let result = (|| -> Result<()> {
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(""))?;
            let found = locate_file_internal("target.el", list![dir.to_str().unwrap(); cx], None, None, env, cx)?;
            assert_eq!(found, Some(dir.join("target.el").to_str().unwrap().to_owned()));
            assert!(locate_file_internal("target.el", list!["."; cx], None, None, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}

#[cfg(test)]
mod symlink_target_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    fn query<'ob>(filename: &str, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
        let filename = cx.add(filename);
        root!(filename, cx);
        super::file_symlink_p(filename, env, cx)
    }

    #[cfg(unix)]
    #[test]
    fn local_targets_preserve_relative_absolute_dangling_and_no_follow() -> Result<()> {
        use std::os::unix::fs::symlink;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-symlink-target-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::write(dir.join("plain"), "untouched")?;
            symlink("plain", dir.join("relative"))?;
            symlink(dir.join("plain"), dir.join("absolute"))?;
            symlink("missing-target", dir.join("dangling"))?;
            symlink("relative", dir.join("chain"))?;
            symlink("cycle", dir.join("cycle"))?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            for (name, expected) in [("relative", "plain"), ("dangling", "missing-target"), ("chain", "relative"), ("cycle", "cycle")] {
                let actual = query(name, env, cx)?;
                assert_eq!(<&str>::try_from(actual)?, expected);
            }
            let actual = query("absolute", env, cx)?;
            assert_eq!(<&str>::try_from(actual)?, dir.join("plain").to_str().unwrap());
            for name in ["plain", "missing", "."] { assert!(query(name, env, cx)?.is_nil()); }
            let absolute = dir.join("relative");
            assert_eq!(<&str>::try_from(query(absolute.to_str().unwrap(), env, cx)?)?, "plain");
            assert_eq!(std::fs::read_to_string(dir.join("plain"))?, "untouched");
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[cfg(unix)]
    #[test]
    fn raw_target_bytes_are_not_lossily_decoded() -> Result<()> {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let path = std::env::temp_dir().join(format!("rune-symlink-bytes-{}-{}", std::process::id(), rand::random::<u64>()));
        symlink(std::ffi::OsStr::from_bytes(b"raw-\xff"), &path)?;
        let result = (|| -> Result<()> {
            let actual = query(path.to_str().unwrap(), env, cx)?;
            let ObjectType::ByteString(actual) = actual.untag() else { panic!("raw target must remain bytes") };
            assert_eq!(&**actual, b"raw-\xff"); Ok(())
        })();
        std::fs::remove_file(path)?; result
    }

    #[test]
    fn handler_receives_operation_expanded_name_and_propagates_return() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-symlink-handler (function (lambda (op name) (garbage-collect) (list op name)))) (let ((default-directory "/rune-symlink-test/") (file-name-handler-alist '(("rune-symlink-test" . rune-symlink-handler)))) (file-symlink-p "virtual")))"#,
            r#"(file-symlink-p "/rune-symlink-test/virtual")"#,
        );
    }

    #[test]
    fn handler_inhibition_is_per_operation_and_errors_propagate() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-symlink-error (function (lambda (op name) (signal 'error '("handler-failure"))))) (let ((file-name-handler-alist '(("rune-symlink-absent" . rune-symlink-error))) (inhibit-file-name-handlers '(rune-symlink-error))) (list (let ((inhibit-file-name-operation 'file-symlink-p)) (file-symlink-p "/rune-symlink-absent/virtual")) (let ((inhibit-file-name-operation 'file-exists-p)) (condition-case nil (file-symlink-p "/rune-symlink-absent/virtual") (error 'propagated))))))"#,
            "(nil propagated)",
        );
    }

    #[test]
    fn lexical_inhibition_is_special_and_relative_default_fails_closed() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-symlink-lexical-handler (function (lambda (op name) 'handled))) (eval '(let ((file-name-handler-alist '(("rune-symlink-lexical" . rune-symlink-lexical-handler))) (inhibit-file-name-handlers '(rune-symlink-lexical-handler))) (list (let ((inhibit-file-name-operation 'file-symlink-p)) (file-symlink-p "/rune-symlink-lexical/missing")) (let ((inhibit-file-name-operation 'file-exists-p)) (file-symlink-p "/rune-symlink-lexical/missing")) (let ((default-directory "relative-base/")) (condition-case nil (file-symlink-p "local") (error 'rejected))))) t))"#,
            "(nil handled rejected)",
        );
    }
}

#[cfg(test)]
mod lexical_filename_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[cfg(unix)]
    #[test]
    fn unix_split_preserves_dot_components_and_repeated_separators() -> Result<()> {
        let roots = RootSet::default(); let cx = Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(&cx)?), cx);
        for (name, directory, basename) in [
            ("", None, ""), ("plain", None, "plain"), (".", None, "."), ("..", None, ".."),
            ("/", Some("/"), ""), ("//", Some("//"), ""),
            ("a/.", Some("a/"), "."), ("a/..", Some("a/"), ".."),
            ("a//b", Some("a//"), "b"), ("./a", Some("./"), "a"),
            ("../a/", Some("../a/"), ""), ("/a/../b", Some("/a/../"), "b"),
            ("é/文", Some("é/"), "文"), ("a\\b", None, "a\\b"),
        ] {
            let input = cx.add(name);
            let actual = file_name_directory(input, env, &cx)?;
            match directory {
                None => assert!(actual.is_nil(), "{name}"),
                Some(expected) => assert_eq!(<&str>::try_from(actual)?, expected, "{name}"),
            }
            assert_eq!(<&str>::try_from(file_name_nondirectory(input, env, &cx)?)?, basename, "{name}");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unibyte_splits_preserve_non_utf8_bytes_and_string_kind() -> Result<()> {
        let roots = RootSet::default(); let cx = Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(&cx)?), cx);
        for (input, prefix, suffix) in [
            (b"raw-\xff/leaf-\xfe".as_slice(), b"raw-\xff/".as_slice(), b"leaf-\xfe".as_slice()),
            (b"a/".as_slice(), b"a/".as_slice(), b"".as_slice()),
        ] {
            let input = cx.add(input.to_vec());
            for (actual, expected) in [(file_name_directory(input, env, &cx)?, prefix), (file_name_nondirectory(input, env, &cx)?, suffix)] {
                let ObjectType::ByteString(bytes) = actual.untag() else { panic!("unibyte representation changed") };
                assert_eq!(&**bytes, expected);
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn lisp_truename_dot_tests_and_type_errors() {
        crate::interpreter::assert_lisp(
            r#"(list (equal (file-name-nondirectory "link/..") "..") (equal (file-name-nondirectory "link/.") ".") (file-name-directory "a//../leaf") (condition-case nil (file-name-directory 3) (wrong-type-argument 'rejected)) (condition-case nil (file-name-nondirectory nil) (wrong-type-argument 'rejected)))"#,
            r#"(t t "a//../" rejected rejected)"#,
        );
        crate::interpreter::assert_lisp(
            r#"(list (condition-case err (file-name-directory 3) (wrong-type-argument err)) (condition-case err (file-name-nondirectory nil) (wrong-type-argument err)))"#,
            "((wrong-type-argument stringp 3) (wrong-type-argument stringp nil))",
        );
    }
}

#[cfg(test)]
mod local_expansion_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[cfg(unix)]
    #[test]
    fn lexical_normalization_preserves_gnu_superroot_and_double_root() {
        for (input, expected) in [
            ("/a/./b", "/a/b"), ("/a/../b", "/b"), ("/a/..", "/"),
            ("/..", "/.."), ("/../x", "/../x"), ("/.", "/"),
            ("//host/a", "//host/a"), ("///host//a", "/host/a"),
            ("/a//b/", "/a/b/"), ("/a/b/.", "/a/b"),
            ("/a/../../x", "/../x"), ("/é/../文", "/文"),
        ] { assert_eq!(normalize_local_path(input.as_bytes()), expected.as_bytes(), "{input}"); }
    }

    #[cfg(unix)]
    #[test]
    fn default_directory_tracks_dynamic_owner_and_explicit_values() {
        crate::interpreter::assert_lisp(
            r#"(progn (setq default-directory "/buffer/base/") (list (expand-file-name "../file") (let ((default-directory "/dynamic/base/")) (expand-file-name "./file")) (expand-file-name "file") (expand-file-name "file" "/explicit/base") (expand-file-name "/a/../absolute") (expand-file-name "")))"#,
            r#"("/buffer/file" "/dynamic/base/file" "/buffer/base/file" "/explicit/base/file" "/absolute" "/buffer/base/")"#,
        );
    }

    #[cfg(unix)]
    #[test]
    fn byte_expansion_is_lossless_and_rejects_invalid_inputs() -> Result<()> {
        let roots = RootSet::default(); let cx = &Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let actual = expand_file_name_lisp(cx.add(b"../raw-\xff".to_vec()), Some(cx.add(b"/a/b/".to_vec())), env, cx)?;
        let ObjectType::ByteString(actual) = actual.untag() else { panic!("raw filename changed kind") };
        assert_eq!(&**actual, b"/a/raw-\xff");
        let mixed = expand_file_name_lisp(cx.add(b"leaf".to_vec()), Some(cx.add("/é/")), env, cx)?;
        assert_eq!(<&str>::try_from(mixed)?, "/é/leaf");
        let mixed = expand_file_name_lisp(cx.add("é"), Some(cx.add(b"/raw-\xff/".to_vec())), env, cx)?;
        let ObjectType::ByteString(mixed) = mixed.untag() else { panic!("mixed raw path changed kind") };
        assert_eq!(&**mixed, b"/raw-\xff/\xc3\xa9");
        assert!(expand_file_name_lisp(cx.add("a\0b"), Some(cx.add("/")), env, cx).is_err());
        assert!(expand_file_name_lisp(cx.add("file"), Some(cx.add("relative/")), env, cx).is_err());
        assert_eq!(<&str>::try_from(expand_file_name_lisp(cx.add("file"), Some(3i64.into()), env, cx)?)?, "/file");
        assert!(expand_file_name_lisp(cx.add("x".repeat(65537)), Some(cx.add("/")), env, cx).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn absolute_paths_ignore_invalid_defaults_and_tilde_scope_is_unchanged() {
        crate::interpreter::assert_lisp(
            r#"(let ((default-directory 7)) (list (expand-file-name "/a/./b") (expand-file-name "relative") (expand-file-name "~/literal/.." "/base/")))"#,
            r#"("/a/b" "/relative" "/base/~/literal/..")"#,
        );
    }
}


#[cfg(all(test, unix))]
mod local_directory_conversion_tests {
    use super::*;

    #[test]
    fn directory_suffixes_preserve_roots_and_dot_components() {
        for (input, expected) in [
            ("", ""), ("/", "/"), ("//", "//"), ("///", "/"), ("////", "/"),
            ("a///", "a"), ("/a///", "/a"), ("//host///", "//host"),
            ("./", "."), ("../", ".."), ("/a/../", "/a/.."), ("é///", "é"),
            ("/x", "/x"), ("a\0b///", "a\0b"),
        ] { assert_eq!(directory_file_name_lexical(input), expected, "{input:?}"); }
    }

    #[test]
    fn as_directory_empty_is_current_not_root() {
        for (input, expected) in [("", "./"), ("a", "a/"), ("/", "/"),
            ("//", "//"), ("a///", "a///"), ("..", "../"), ("é", "é/")]
        { assert_eq!(file_name_as_directory_lexical(input), expected, "{input:?}"); }
    }

    #[test]
    fn lisp_directory_conversion_composition() {
        crate::interpreter::assert_lisp(
            r#"(list (file-name-as-directory "") (directory-file-name "//") (directory-file-name "path///") (file-name-as-directory (directory-file-name "a/../")))"#,
            r#"("./" "//" "path" "a/../")"#,
        );
    }
}


#[cfg(all(test, unix))]
mod filename_concat_filter_tests {
    #[test]
    fn empty_and_nil_components_do_not_create_slashes() {
        crate::interpreter::assert_lisp(
            r#"(list (file-name-concat nil) (file-name-concat "") (file-name-concat nil "" nil) (file-name-concat "" "leaf") (file-name-concat "base" "" nil) (file-name-concat nil "base" nil "leaf" ""))"#,
            r#"("" "" "" "leaf" "base" "base/leaf")"#,
        );
    }

    #[test]
    fn absolute_components_and_dot_segments_remain_lexical() {
        crate::interpreter::assert_lisp(
            r#"(list (file-name-concat "base" "/absolute") (file-name-concat "base/" "/absolute") (file-name-concat "//" "host" "./" "../leaf") (file-name-concat "é" "資料") (file-name-concat "base" "leaf///"))"#,
            r#"("base//absolute" "base//absolute" "//host/./../leaf" "é/資料" "base/leaf///")"#,
        );
    }

    #[test]
    fn invalid_components_signal_stringp_with_original_operand() {
        crate::interpreter::assert_lisp(
            r#"(list (condition-case err (file-name-concat 3) (wrong-type-argument err)) (condition-case err (file-name-concat nil "" 7) (wrong-type-argument err)) (condition-case err (file-name-concat "base" t) (wrong-type-argument err)))"#,
            "((wrong-type-argument stringp 3) (wrong-type-argument stringp 7) (wrong-type-argument stringp t))",
        );
    }
}


#[cfg(all(test, unix))]
mod directory_predicate_byte_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn raw_bytes_use_only_final_separator_without_decoding() -> Result<()> {
        let roots = RootSet::default(); let cx = &Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        for (bytes, expected) in [
            (b"".as_slice(), false), (b"/".as_slice(), true),
            (b"raw-\xff/".as_slice(), true), (b"raw-\xff".as_slice(), false),
            (b"a/\0".as_slice(), false), (b"a\0/".as_slice(), true),
            (b"a\\".as_slice(), false),
        ] { assert_eq!(directory_name_p(cx.add(bytes.to_vec()), env, cx)?, expected); }
        Ok(())
    }

    #[test]
    fn string_predicate_is_lexical_for_empty_unicode_and_dot_names() {
        crate::interpreter::assert_lisp(
            r#"(list (directory-name-p "") (directory-name-p "/") (directory-name-p "//") (directory-name-p "é/") (directory-name-p "é") (directory-name-p "./") (directory-name-p "..") (directory-name-p "missing/"))"#,
            "(nil t t t nil t nil t)",
        );
    }

    #[test]
    fn nonstrings_signal_exact_stringp_payload() {
        crate::interpreter::assert_lisp(
            r#"(list (condition-case err (directory-name-p 3) (wrong-type-argument err)) (condition-case err (directory-name-p nil) (wrong-type-argument err)) (condition-case err (directory-name-p '(x)) (wrong-type-argument err)))"#,
            "((wrong-type-argument stringp 3) (wrong-type-argument stringp nil) (wrong-type-argument stringp (x)))",
        );
    }
}


#[cfg(all(test, unix))]
mod file_exists_local_tests {
    use super::*;
    use crate::core::gc::{RootSet, Rto};
    use rune_core::macros::root;

    fn query<'ob>(name: &Rto<Object>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
        super::file_exists_p(name, env, cx)
    }

    #[test]
    fn local_lookup_uses_owner_default_and_follows_links() -> Result<()> {
        use std::os::unix::fs::symlink;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-exists-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::write(dir.join("plain"), "unchanged")?;
            symlink("plain", dir.join("link"))?;
            symlink("missing", dir.join("dangling"))?;
            symlink("loop", dir.join("loop"))?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            for (name, expected) in [("plain", true), ("link", true), (".", true), ("", true),
                ("missing", false), ("dangling", false), ("loop", false), ("plain/child", false)] {
                let name = cx.add(name); root!(name, cx);
                assert_eq!(!query(name, env, cx)?.is_nil(), expected);
            }
            let name = cx.add(dir.join("plain").to_str().unwrap()); root!(name, cx);
            assert!(!query(name, env, cx)?.is_nil());
            assert_eq!(std::fs::read_to_string(dir.join("plain"))?, "unchanged");
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn raw_unix_paths_preserve_bytes_and_fail_closed_with_handlers() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-exists-bytes-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            let file = dir.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
            std::fs::write(&file, "kept")?;
            let name = cx.add(file.as_os_str().as_bytes().to_vec()); root!(name, cx);
            assert!(!query(name, env, cx)?.is_nil());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, list![NIL; cx])?;
            assert!(query(name, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn handler_receives_expanded_name_and_returns_after_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-exists-handler (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-exists-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (garbage-collect) (list op name)))))) (let ((default-directory "/rune-exists-handler/") (file-name-handler-alist '(("rune-exists-handler" . rune-exists-handler)))) (file-exists-p "./virtual")))"#,
            r#"(file-exists-p "/rune-exists-handler/virtual")"#,
        );
    }

    #[test]
    fn operation_inhibition_and_handler_errors_are_preserved() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-exists-error (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-exists-error inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (signal 'error '("exists-handler"))))))) (let ((file-name-handler-alist '(("rune-exists-absent" . rune-exists-error))) (inhibit-file-name-handlers '(rune-exists-error))) (list (let ((inhibit-file-name-operation 'file-exists-p)) (file-exists-p "/rune-exists-absent/missing")) (let ((inhibit-file-name-operation 'file-symlink-p)) (condition-case err (file-exists-p "/rune-exists-absent/missing") (error err))))))"#,
            r#"(nil (error "exists-handler"))"#,
        );
    }

    #[test]
    fn invalid_type_is_catchable_and_relative_default_is_rejected() -> Result<()> {
        crate::interpreter::assert_lisp(
            r#"(condition-case err (file-exists-p 3) (wrong-type-argument err))"#,
            "(wrong-type-argument stringp 3)",
        );
        // The bounded expansion rejection is currently a Rust error, not a
        // catchable Lisp condition. Assert that boundary without claiming more.
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        env.set_var(sym::DEFAULT_DIRECTORY, cx.add("relative"))?;
        let name = cx.add("file"); root!(name, cx);
        let error = query(name, env, cx).unwrap_err();
        assert!(error.to_string().contains("Relative default-directory is unsupported"));
        Ok(())
    }
}


#[cfg(all(test, unix))]
mod file_directory_owner_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn owner_default_empty_names_and_directory_links() -> Result<()> {
        use std::os::unix::fs::symlink;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-directory-owner-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::create_dir(dir.join("child"))?;
            std::fs::write(dir.join("plain"), "kept")?;
            symlink("child", dir.join("dir-link"))?;
            symlink("plain", dir.join("file-link"))?;
            symlink("missing", dir.join("dangling"))?;
            symlink("loop", dir.join("loop"))?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            for (name, expected) in [("", true), (".", true), ("child///", true), ("dir-link", true),
                ("plain", false), ("file-link", false), ("dangling", false), ("loop", false), ("missing", false)] {
                let name = cx.add(name); root!(name, cx);
                assert_eq!(!file_directory_p(name, env, cx)?.is_nil(), expected);
            }
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.join("missing").to_str().unwrap()))?;
            let empty = cx.add(""); root!(empty, cx);
            assert!(file_directory_p(empty, env, cx)?.is_nil());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn raw_directory_paths_preserve_bytes() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-directory-raw-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            let child = dir.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
            std::fs::create_dir(&child)?;
            let mut bytes = child.as_os_str().as_bytes().to_vec(); bytes.extend_from_slice(b"///");
            let name = cx.add(bytes); root!(name, cx);
            assert!(!file_directory_p(name, env, cx)?.is_nil());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, list![NIL; cx])?;
            assert!(file_directory_p(name, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn distinct_operation_and_suffix_handlers_survive_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-directory-handler (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-directory-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (if (eq op 'directory-file-name) (let ((inhibit-file-name-handlers (cons 'rune-directory-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'directory-file-name)) (directory-file-name name)) (progn (garbage-collect) (list op name))))))) (let ((default-directory "/rune-directory-handler/") (file-name-handler-alist '(("rune-directory-handler" . rune-directory-handler)))) (list (file-directory-p "child///") (file-exists-p "child///"))))"#,
            r#"((file-directory-p "/rune-directory-handler/child") (file-exists-p "/rune-directory-handler/child/"))"#,
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-root-directory-handler (function (lambda (op name &optional base) (if (or (eq op 'expand-file-name) (eq op 'directory-file-name)) name (list op name))))) (let ((file-name-handler-alist '(("/" . rune-root-directory-handler)))) (list (file-directory-p "/") (file-directory-p "//"))))"#,
            r#"((file-directory-p "/") (file-directory-p "//"))"#,
        );
    }

    #[test]
    fn directory_operation_inhibition_errors_and_type_payload() -> anyhow::Result<()> {
        let fixture = super::directory_expansion_dispatch_tests::DirectoryFixture::new()?;
        let missing = fixture.0.join("rune-directory-absent");
        assert!(!missing.exists());
        crate::interpreter::assert_lisp(
            &format!(r#"(progn (defalias 'rune-directory-error (function (lambda (op name &optional base) (if (eq op 'expand-file-name) name (signal 'error '("directory-handler")))))) (let ((file-name-handler-alist '(("rune-directory-absent" . rune-directory-error))) (inhibit-file-name-handlers '(rune-directory-error))) (list (let ((inhibit-file-name-operation 'file-directory-p)) (file-directory-p {missing:?})) (let ((inhibit-file-name-operation 'file-exists-p)) (condition-case err (file-directory-p {missing:?}) (error err))) (condition-case err (file-directory-p 3) (wrong-type-argument err)))))"#, missing = missing.to_str().unwrap()),
            r#"(nil (error "directory-handler") (wrong-type-argument stringp 3))"#,
        );
        assert!(!missing.exists());
        Ok(())
    }
}


#[cfg(all(test, target_os = "linux"))]
mod file_readable_linux_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn effective_read_lookup_follows_links_and_owner_default() -> Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-readable-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::write(dir.join("plain"), "kept")?;
            std::fs::set_permissions(dir.join("plain"), std::fs::Permissions::from_mode(0o600))?;
            symlink("plain", dir.join("link"))?;
            symlink("missing", dir.join("dangling"))?;
            symlink("loop", dir.join("loop"))?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            // No mode-000 denial assertion: root/capabilities can legitimately
            // read such a file. These outcomes do not depend on running as root.
            for (name, expected) in [("plain", true), ("link", true), ("", true),
                ("dangling", false), ("loop", false), ("missing", false), ("plain/child", false)] {
                let name = cx.add(name); root!(name, cx);
                assert_eq!(!file_readable_p(name, env, cx)?.is_nil(), expected);
            }
            assert_eq!(std::fs::read_to_string(dir.join("plain"))?, "kept");
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn raw_readable_paths_and_invalid_nul_do_not_truncate() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-readable-raw-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            let path = dir.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
            std::fs::write(&path, "kept")?;
            let bytes = path.as_os_str().as_bytes().to_vec();
            let name = cx.add(bytes.clone()); root!(name, cx);
            assert!(!file_readable_p(name, env, cx)?.is_nil());
            let mut nul = bytes; nul.extend_from_slice(b"\0suffix");
            let invalid = cx.add(nul.clone()); root!(invalid, cx);
            assert!(file_readable_p(invalid, env, cx).is_err());
            assert!(effective_readable_access(Path::new(std::ffi::OsStr::from_bytes(&nul))).is_err());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, list![NIL; cx])?;
            assert!(file_readable_p(name, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn readable_handler_preserves_operation_result_and_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-readable-handler (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-readable-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (garbage-collect) (list op name)))))) (let ((default-directory "/rune-readable-handler/") (file-name-handler-alist '(("rune-readable-handler" . rune-readable-handler)))) (file-readable-p "./virtual/")))"#,
            r#"(file-readable-p "/rune-readable-handler/virtual/")"#,
        );
    }

    #[test]
    fn readable_inhibition_errors_and_type_payload() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-readable-error (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-readable-error inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (signal 'error '("readable-handler"))))))) (let ((file-name-handler-alist '(("rune-readable-absent" . rune-readable-error))) (inhibit-file-name-handlers '(rune-readable-error))) (list (let ((inhibit-file-name-operation 'file-readable-p)) (file-readable-p "/rune-readable-absent/missing")) (let ((inhibit-file-name-operation 'file-exists-p)) (condition-case err (file-readable-p "/rune-readable-absent/missing") (error err))) (condition-case err (file-readable-p 3) (wrong-type-argument err)))))"#,
            r#"(nil (error "readable-handler") (wrong-type-argument stringp 3))"#,
        );
    }
}


#[cfg(all(test, target_os = "linux"))]
mod accessible_directory_linux_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn owner_directory_search_is_not_file_readability() -> Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-directory-access-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::create_dir(dir.join("search-only"))?;
            std::fs::set_permissions(dir.join("search-only"), std::fs::Permissions::from_mode(0o100))?;
            std::fs::write(dir.join("plain"), "kept")?;
            symlink("search-only", dir.join("link"))?;
            symlink("missing", dir.join("dangling"))?;
            symlink("loop", dir.join("loop"))?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            // Search-only directory succeeds for ordinary owners and root;
            // no root-dependent unreadability or mode-000 denial assertion.
            for (name, expected) in [("", true), ("search-only", true), ("search-only/", true), ("link", true),
                ("plain", false), ("plain/", false), ("missing", false), ("dangling", false), ("loop", false)] {
                let name = cx.add(name); root!(name, cx);
                assert_eq!(!file_accessible_directory_p(name, env, cx)?.is_nil(), expected);
            }
            Ok(())
        })();
        let _ = std::fs::set_permissions(dir.join("search-only"), std::fs::Permissions::from_mode(0o700));
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn raw_paths_and_nul_use_lossless_directory_probe() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-directory-access-raw-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            let path = dir.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
            std::fs::create_dir(&path)?;
            let mut bytes = path.as_os_str().as_bytes().to_vec();
            let name = cx.add(bytes.clone()); root!(name, cx);
            assert!(!file_accessible_directory_p(name, env, cx)?.is_nil());
            bytes.extend_from_slice(b"\0suffix");
            let invalid = cx.add(bytes.clone()); root!(invalid, cx);
            assert!(file_accessible_directory_p(invalid, env, cx).is_err());
            assert!(effective_directory_access(Path::new(std::ffi::OsStr::from_bytes(&bytes))).is_err());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, list![NIL; cx])?;
            assert!(file_accessible_directory_p(name, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn handler_preserves_suffix_roots_result_and_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-access-directory-handler (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-access-directory-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (garbage-collect) (list op name)))))) (let ((default-directory "/rune-access-directory/") (file-name-handler-alist '(("/" . rune-access-directory-handler)))) (list (file-accessible-directory-p "child/") (file-accessible-directory-p "/") (file-accessible-directory-p "//"))))"#,
            r#"((file-accessible-directory-p "/rune-access-directory/child/") (file-accessible-directory-p "/") (file-accessible-directory-p "//"))"#,
        );
    }

    #[test]
    fn inhibition_error_and_type_payload_use_directory_access_operation() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-access-directory-error (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-access-directory-error inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (signal 'error '("directory-access"))))))) (let ((file-name-handler-alist '(("rune-access-directory-absent" . rune-access-directory-error))) (inhibit-file-name-handlers '(rune-access-directory-error))) (list (let ((inhibit-file-name-operation 'file-accessible-directory-p)) (file-accessible-directory-p "/rune-access-directory-absent/missing")) (let ((inhibit-file-name-operation 'file-directory-p)) (condition-case err (file-accessible-directory-p "/rune-access-directory-absent/missing") (error err))) (condition-case err (file-accessible-directory-p 3) (wrong-type-argument err)))))"#,
            r#"(nil (error "directory-access") (wrong-type-argument stringp 3))"#,
        );
    }
}


#[cfg(all(test, target_os = "linux"))]
mod file_writable_linux_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn writable_existing_or_creatable_without_creating_files() -> Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-writable-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            std::fs::write(dir.join("plain"), "kept")?;
            std::fs::set_permissions(dir.join("plain"), std::fs::Permissions::from_mode(0o600))?;
            symlink("plain", dir.join("link"))?;
            symlink("missing-target", dir.join("dangling"))?;
            symlink(".", dir.join("parent-link"))?;
            symlink("loop", dir.join("loop"))?;
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            // Success cases work for the owner or root; ENOTDIR/ELOOP cases
            // distinguish the ENOENT-only fallback without permission guesses.
            for (name, expected) in [("", true), ("plain", true), ("link", true), ("missing", true),
                ("dangling", true), ("parent-link/new", true), ("missing-parent/new", false),
                ("missing/", false), ("plain/child", false), ("loop", false)] {
                let name = cx.add(name); root!(name, cx);
                assert_eq!(!file_writable_p(name, env, cx)?.is_nil(), expected);
            }
            for absent in ["missing", "missing-target", "new", "missing-parent"] {
                assert!(!dir.join(absent).exists(), "access probe created {absent}");
            }
            assert_eq!(std::fs::read_to_string(dir.join("plain"))?, "kept");
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn raw_missing_child_parent_and_nul_are_byte_exact() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-writable-raw-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            let parent = dir.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
            std::fs::create_dir(&parent)?;
            let path = parent.join(std::ffi::OsStr::from_bytes(b"new-\xfe"));
            let bytes = path.as_os_str().as_bytes().to_vec();
            let name = cx.add(bytes.clone()); root!(name, cx);
            assert!(!file_writable_p(name, env, cx)?.is_nil());
            assert!(!path.exists());
            let mut invalid = bytes; invalid.extend_from_slice(b"\0suffix");
            let nul = cx.add(invalid.clone()); root!(nul, cx);
            assert!(file_writable_p(nul, env, cx).is_err());
            assert!(effective_writable_access(Path::new(std::ffi::OsStr::from_bytes(&invalid))).is_err());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, list![NIL; cx])?;
            assert!(file_writable_p(name, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn handler_receives_writable_operation_and_keeps_suffix_after_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-writable-handler (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-writable-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (garbage-collect) (list op name)))))) (let ((default-directory "/rune-writable-handler/") (file-name-handler-alist '(("rune-writable-handler" . rune-writable-handler)))) (file-writable-p "./virtual/")))"#,
            r#"(file-writable-p "/rune-writable-handler/virtual/")"#,
        );
    }

    #[test]
    fn handler_inhibition_errors_and_type_payload_remain_distinct() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-writable-error (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-writable-error inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (progn (signal 'error '("writable-handler"))))))) (let ((file-name-handler-alist '(("rune-writable-absent" . rune-writable-error))) (inhibit-file-name-handlers '(rune-writable-error))) (list (let ((inhibit-file-name-operation 'file-writable-p)) (file-writable-p "/rune-writable-absent/missing-parent/child")) (let ((inhibit-file-name-operation 'file-readable-p)) (condition-case err (file-writable-p "/rune-writable-absent/missing-parent/child") (error err))) (condition-case err (file-writable-p 3) (wrong-type-argument err)))))"#,
            r#"(nil (error "writable-handler") (wrong-type-argument stringp 3))"#,
        );
    }
}


#[cfg(all(test, unix))]
mod file_regular_local_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn regular_files_follow_links_and_exclude_directories_and_fifo() -> Result<()> {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-regular-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            std::fs::write(dir.join("plain"), "kept")?;
            std::fs::create_dir(dir.join("child"))?;
            symlink("plain", dir.join("link"))?;
            symlink("child", dir.join("dir-link"))?;
            symlink("missing", dir.join("dangling"))?;
            symlink("loop", dir.join("loop"))?;
            let fifo = std::ffi::CString::new(dir.join("pipe").as_os_str().as_bytes())?;
            // SAFETY: owned NUL-terminated fixture pathname lives through mkfifo.
            // Nothing opens the FIFO; the predicate must classify without blocking.
            ensure!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } == 0, "Cannot create FIFO fixture");
            env.set_var(sym::DEFAULT_DIRECTORY, cx.add(dir.to_str().unwrap()))?;
            for (name, expected) in [("plain", true), ("plain/", true), ("link", true),
                ("child", false), ("dir-link", false), ("", false), ("pipe", false),
                ("dangling", false), ("loop", false), ("missing", false)] {
                let name = cx.add(name); root!(name, cx);
                assert_eq!(!file_regular_p(name, env, cx)?.is_nil(), expected);
            }
            assert_eq!(std::fs::read_to_string(dir.join("plain"))?, "kept");
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn raw_regular_paths_keep_bytes_and_handler_limit() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let dir = std::env::temp_dir().join(format!("rune-regular-raw-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            let path = dir.join(std::ffi::OsStr::from_bytes(b"raw-\xff"));
            std::fs::write(&path, "kept")?;
            let mut bytes = path.as_os_str().as_bytes().to_vec(); bytes.extend_from_slice(b"///");
            let name = cx.add(bytes); root!(name, cx);
            assert!(!file_regular_p(name, env, cx)?.is_nil());
            let invalid = cx.add(b"invalid\0suffix".to_vec()); root!(invalid, cx);
            assert!(file_regular_p(invalid, env, cx).is_err());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, list![NIL; cx])?;
            assert!(file_regular_p(name, env, cx).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }

    #[test]
    fn regular_handler_receives_converted_suffix_and_roots_after_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-regular-handler (function (lambda (op name &optional base) (if (eq op 'expand-file-name) (let ((inhibit-file-name-handlers (cons 'rune-regular-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base)) (if (eq op 'directory-file-name) (let ((inhibit-file-name-handlers (cons 'rune-regular-handler inhibit-file-name-handlers)) (inhibit-file-name-operation 'directory-file-name)) (directory-file-name name)) (progn (garbage-collect) (list op name))))))) (let ((default-directory "/rune-regular-handler/") (file-name-handler-alist '(("/" . rune-regular-handler)))) (list (file-regular-p "./virtual///") (file-regular-p "/") (file-regular-p "//"))))"#,
            r#"((file-regular-p "/rune-regular-handler/virtual") (file-regular-p "/") (file-regular-p "//"))"#,
        );
    }

    #[test]
    fn regular_inhibition_errors_and_type_payload_are_operation_specific() -> anyhow::Result<()> {
        let fixture = super::directory_expansion_dispatch_tests::DirectoryFixture::new()?;
        let missing = fixture.0.join("rune-regular-absent");
        assert!(!missing.exists());
        crate::interpreter::assert_lisp(
            &format!(r#"(progn (defalias 'rune-regular-error (function (lambda (op name &optional base) (if (eq op 'expand-file-name) name (signal 'error '("regular-handler")))))) (let ((file-name-handler-alist '(("rune-regular-absent" . rune-regular-error))) (inhibit-file-name-handlers '(rune-regular-error))) (list (let ((inhibit-file-name-operation 'file-regular-p)) (file-regular-p {missing:?})) (let ((inhibit-file-name-operation 'file-directory-p)) (condition-case err (file-regular-p {missing:?}) (error err))) (condition-case err (file-regular-p 3) (wrong-type-argument err)))))"#, missing = missing.to_str().unwrap()),
            r#"(nil (error "regular-handler") (wrong-type-argument stringp 3))"#,
        );
        assert!(!missing.exists());
        Ok(())
    }
}


#[cfg(all(test, unix))]
mod expansion_handler_dispatch_tests {
    #[test]
    fn native_registration_preserves_expand_and_write_region() {
        crate::interpreter::assert_lisp(
            r#"(let ((default-directory "/registration/") (file-name-handler-alist nil)) (list (fboundp 'expand-file-name) (fboundp 'write-region) (eq (symbol-function 'expand-file-name) (symbol-function 'write-region)) (expand-file-name "leaf") (expand-file-name "leaf" "/explicit/")))"#,
            r#"(t t nil "/registration/leaf" "/explicit/leaf")"#,
        );
    }

    #[test]
    fn name_handler_sees_original_arguments_once_and_survives_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-expand-log nil) (defalias 'rune-expand-handler (function (lambda (op name base) (garbage-collect) (setq rune-expand-log (cons (list op name base) rune-expand-log)) "/handled"))) (let ((rune-expand-log nil) (file-name-handler-alist '(("MAGIC" . rune-expand-handler)))) (list (expand-file-name "MAGIC/../leaf" "/MAGIC/base/") rune-expand-log)))"#,
            r#"("/handled" ((expand-file-name "MAGIC/../leaf" "/MAGIC/base/")))"#,
        );
    }

    #[test]
    fn default_handler_uses_owner_or_explicit_default_even_for_absolute_name() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-expand-default-log nil) (defalias 'rune-expand-default-handler (function (lambda (op name base) (setq rune-expand-default-log (list op name base)) "/default-handled"))) (let ((default-directory "/MAGIC/owner/") (rune-expand-default-log nil) (file-name-handler-alist '(("MAGIC" . rune-expand-default-handler)))) (list (expand-file-name "leaf") rune-expand-default-log (expand-file-name "/absolute" "/MAGIC/explicit/") rune-expand-default-log)))"#,
            r#"("/default-handled" (expand-file-name "leaf" "/MAGIC/owner/") "/default-handled" (expand-file-name "/absolute" "/MAGIC/explicit/"))"#,
        );
    }

    #[test]
    fn recursion_uses_per_operation_handler_inhibition() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-expand-delegate (function (lambda (op name base) (let ((inhibit-file-name-handlers (cons 'rune-expand-delegate inhibit-file-name-handlers)) (inhibit-file-name-operation 'expand-file-name)) (expand-file-name name base))))) (let ((file-name-handler-alist '(("MAGIC" . rune-expand-delegate))) (inhibit-file-name-handlers '(rune-expand-delegate)) (inhibit-file-name-operation 'file-exists-p)) (expand-file-name "MAGIC/../leaf" "/base/")))"#,
            r#""/base/leaf""#,
        );
    }

    #[test]
    fn handler_errors_and_nonstring_returns_are_catchable() {
        crate::interpreter::assert_lisp(
            r#"(progn (defalias 'rune-expand-bad (function (lambda (op name base) 3))) (defalias 'rune-expand-error (function (lambda (op name base) (signal 'error '("expand-handler"))))) (list (let ((file-name-handler-alist '(("MAGIC" . rune-expand-bad)))) (condition-case err (expand-file-name "MAGIC" "/") (error err))) (let ((file-name-handler-alist '(("MAGIC" . rune-expand-error)))) (condition-case err (expand-file-name "MAGIC" "/") (error err)))))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (error "expand-handler"))"#,
        );
    }

    #[test]
    fn nil_supplied_default_and_invalid_name_payload_are_preserved() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-expand-nil-log t) (defalias 'rune-expand-nil-handler (function (lambda (op name base) (setq rune-expand-nil-log base) "/handled"))) (let ((file-name-handler-alist '(("MAGIC" . rune-expand-nil-handler)))) (list (expand-file-name "MAGIC") rune-expand-nil-log (condition-case err (expand-file-name 3) (wrong-type-argument err)))))"#,
            r#"("/handled" nil (wrong-type-argument stringp 3))"#,
        );
        crate::interpreter::assert_lisp(
            r#"(let ((name (concat "x" (make-string 1 0) "y"))) (condition-case err (expand-file-name name) (wrong-type-argument (list (car err) (car (cdr err)) (eq (car (cdr (cdr err))) name)))))"#,
            "(wrong-type-argument filenamep t)",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-expand-explicit-base nil) (defalias 'rune-expand-nul-default-handler (function (lambda (op name base) (garbage-collect) (setq rune-expand-explicit-base base) "/handled"))) (let ((base (concat "/MAGIC/" (make-string 1 0) "suffix")) (file-name-handler-alist '(("MAGIC" . rune-expand-nul-default-handler)))) (list (expand-file-name "leaf" base) (eq rune-expand-explicit-base base) (let ((default-directory base)) (condition-case err (expand-file-name "leaf") (wrong-type-argument (list (car err) (car (cdr err)) (eq (car (cdr (cdr err))) base))))))))"#,
            r#"("/handled" t (wrong-type-argument filenamep t))"#,
        );
    }
}


#[cfg(all(test, unix))]
mod exists_expansion_dispatch_tests {
    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-exists-stage-log nil) (defalias 'rune-exists-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-exists-stage-log (cons (list op name base) rune-exists-stage-log)) "/RESOLVED/target"))) (defalias 'rune-exists-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-exists-stage-log (cons (list op name) rune-exists-stage-log)) 'exists-answer))) (let ((rune-exists-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-exists-expand-stage) ("RESOLVED" . rune-exists-resolved-stage)))) (list (file-exists-p "MAGIC/leaf") rune-exists-stage-log)))"#,
            r#"(exists-answer ((file-exists-p "/RESOLVED/target") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-exists-inhibit-log nil) (defalias 'rune-exists-inhibit-expand (function (lambda (op name &optional base) (setq rune-exists-inhibit-log (cons op rune-exists-inhibit-log)) "/rune-exists-resolved-absent/target"))) (defalias 'rune-exists-inhibit-final (function (lambda (op name) (setq rune-exists-inhibit-log (cons op rune-exists-inhibit-log)) 'should-not-run))) (let ((rune-exists-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-exists-inhibit-expand) ("rune-exists-resolved-absent" . rune-exists-inhibit-final))) (inhibit-file-name-handlers '(rune-exists-inhibit-expand rune-exists-inhibit-final)) (inhibit-file-name-operation 'file-exists-p)) (list (file-exists-p "MAGIC") rune-exists-inhibit-log)))"#,
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-exists-only-log nil) (defalias 'rune-exists-only-handler (function (lambda (op name &optional base) (setq rune-exists-only-log (cons (list op name) rune-exists-only-log)) 'exists-answer))) (let ((rune-exists-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-exists-only-handler))) (inhibit-file-name-handlers '(rune-exists-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-exists-p "/MAGIC/original") rune-exists-only-log)))"#,
            r#"(exists-answer ((file-exists-p "/MAGIC/original")))"#,
        );
    }

    #[test]
    fn expansion_errors_stop_before_existence_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-exists-error-stage-log nil) (defalias 'rune-exists-invalid-expand (function (lambda (op name &optional base) (setq rune-exists-error-stage-log (cons op rune-exists-error-stage-log)) 3))) (let ((rune-exists-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-exists-invalid-expand)))) (list (condition-case err (file-exists-p "MAGIC") (error err)) rune-exists-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_actual_lookup_without_creating_or_rewriting() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("rune-exists-expand-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> anyhow::Result<()> {
            let present = dir.join("present"); let missing = dir.join("missing");
            std::fs::write(&present, "kept")?;
            let source = format!(r#"(progn (defalias 'rune-exists-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/present") {:?} {:?})))) (let ((file-name-handler-alist '(("SOURCE" . rune-exists-redirect)))) (list (file-exists-p "SOURCE/present") (file-exists-p "SOURCE/missing"))))"#, present.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(t nil)");
            assert_eq!(std::fs::read_to_string(&present)?, "kept");
            assert!(!missing.exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}


#[cfg(all(test, target_os = "linux"))]
mod readable_expansion_dispatch_tests {
    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-readable-stage-log nil) (defalias 'rune-readable-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-readable-stage-log (cons (list op name base) rune-readable-stage-log)) "/RESOLVED/target"))) (defalias 'rune-readable-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-readable-stage-log (cons (list op name) rune-readable-stage-log)) 'readable-answer))) (let ((rune-readable-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-readable-expand-stage) ("RESOLVED" . rune-readable-resolved-stage)))) (list (file-readable-p "MAGIC/leaf") rune-readable-stage-log)))"#,
            r#"(readable-answer ((file-readable-p "/RESOLVED/target") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-readable-inhibit-log nil) (defalias 'rune-readable-inhibit-expand (function (lambda (op name &optional base) (setq rune-readable-inhibit-log (cons op rune-readable-inhibit-log)) "/rune-readable-resolved-absent/target"))) (defalias 'rune-readable-inhibit-final (function (lambda (op name) (setq rune-readable-inhibit-log (cons op rune-readable-inhibit-log)) 'should-not-run))) (let ((rune-readable-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-readable-inhibit-expand) ("rune-readable-resolved-absent" . rune-readable-inhibit-final))) (inhibit-file-name-handlers '(rune-readable-inhibit-expand rune-readable-inhibit-final)) (inhibit-file-name-operation 'file-readable-p)) (list (file-readable-p "MAGIC") rune-readable-inhibit-log)))"#,
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-readable-only-log nil) (defalias 'rune-readable-only-handler (function (lambda (op name &optional base) (setq rune-readable-only-log (cons (list op name) rune-readable-only-log)) 'readable-answer))) (let ((rune-readable-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-readable-only-handler))) (inhibit-file-name-handlers '(rune-readable-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-readable-p "/MAGIC/original") rune-readable-only-log)))"#,
            r#"(readable-answer ((file-readable-p "/MAGIC/original")))"#,
        );
    }

    #[test]
    fn expansion_errors_stop_before_readability_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-readable-error-stage-log nil) (defalias 'rune-readable-invalid-expand (function (lambda (op name &optional base) (setq rune-readable-error-stage-log (cons op rune-readable-error-stage-log)) 3))) (let ((rune-readable-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-readable-invalid-expand)))) (list (condition-case err (file-readable-p "MAGIC") (error err)) rune-readable-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_actual_lookup_without_creating_or_rewriting() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("rune-readable-expand-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> anyhow::Result<()> {
            let present = dir.join("present"); let missing = dir.join("missing");
            std::fs::write(&present, "kept")?;
            let source = format!(r#"(progn (defalias 'rune-readable-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/present") {:?} {:?})))) (let ((file-name-handler-alist '(("SOURCE" . rune-readable-redirect)))) (list (file-readable-p "SOURCE/present") (file-readable-p "SOURCE/missing"))))"#, present.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(t nil)");
            assert_eq!(std::fs::read_to_string(&present)?, "kept");
            assert!(!missing.exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}


#[cfg(all(test, target_os = "linux"))]
mod executable_handler_pipeline_tests {
    #[test]
    fn registration_and_type_payloads_remain_native() {
        crate::interpreter::assert_lisp(
            r#"(list (fboundp 'file-executable-p) (condition-case err (file-executable-p 3) (wrong-type-argument err)) (condition-case err (file-executable-p "a\0b") (wrong-type-argument (list (car err) (car (cdr err))))))"#,
            "(t (wrong-type-argument stringp 3) (wrong-type-argument filenamep))",
        );
    }

    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-executable-stage-log nil) (defalias 'rune-executable-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-executable-stage-log (cons (list op name base) rune-executable-stage-log)) "/RESOLVED/target"))) (defalias 'rune-executable-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-executable-stage-log (cons (list op name) rune-executable-stage-log)) 'executable-answer))) (let ((rune-executable-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-executable-expand-stage) ("RESOLVED" . rune-executable-resolved-stage)))) (list (file-executable-p "MAGIC/leaf") rune-executable-stage-log)))"#,
            r#"(executable-answer ((file-executable-p "/RESOLVED/target") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-executable-inhibit-log nil) (defalias 'rune-executable-inhibit-expand (function (lambda (op name &optional base) (setq rune-executable-inhibit-log (cons op rune-executable-inhibit-log)) "/rune-executable-resolved-absent/target"))) (defalias 'rune-executable-inhibit-final (function (lambda (op name) (setq rune-executable-inhibit-log (cons op rune-executable-inhibit-log)) 'should-not-run))) (let ((rune-executable-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-executable-inhibit-expand) ("rune-executable-resolved-absent" . rune-executable-inhibit-final))) (inhibit-file-name-handlers '(rune-executable-inhibit-expand rune-executable-inhibit-final)) (inhibit-file-name-operation 'file-executable-p)) (list (file-executable-p "MAGIC") rune-executable-inhibit-log)))"#,
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-executable-only-log nil) (defalias 'rune-executable-only-handler (function (lambda (op name &optional base) (setq rune-executable-only-log (cons (list op name) rune-executable-only-log)) 'executable-answer))) (let ((rune-executable-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-executable-only-handler))) (inhibit-file-name-handlers '(rune-executable-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-executable-p "/MAGIC/original") rune-executable-only-log)))"#,
            r#"(executable-answer ((file-executable-p "/MAGIC/original")))"#,
        );
    }

    #[test]
    fn expansion_errors_stop_before_executable_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-executable-error-stage-log nil) (defalias 'rune-executable-invalid-expand (function (lambda (op name &optional base) (setq rune-executable-error-stage-log (cons op rune-executable-error-stage-log)) 3))) (let ((rune-executable-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-executable-invalid-expand)))) (list (condition-case err (file-executable-p "MAGIC") (error err)) rune-executable-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_actual_lookup_without_creating_or_rewriting() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("rune-executable-expand-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> anyhow::Result<()> {
            let present = dir.join("present"); let missing = dir.join("missing");
            std::fs::write(&present, "kept")?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&present, std::fs::Permissions::from_mode(0o700))?;
            let source = format!(r#"(progn (defalias 'rune-executable-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/present") {:?} {:?})))) (let ((file-name-handler-alist '(("SOURCE" . rune-executable-redirect)))) (list (file-executable-p "SOURCE/present") (file-executable-p "SOURCE/missing"))))"#, present.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(t nil)");
            assert_eq!(std::fs::read_to_string(&present)?, "kept");
            assert!(!missing.exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}


#[cfg(all(test, target_os = "linux"))]
mod writable_expansion_dispatch_tests {
    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-writable-stage-log nil) (defalias 'rune-writable-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-writable-stage-log (cons (list op name base) rune-writable-stage-log)) "/RESOLVED/target"))) (defalias 'rune-writable-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-writable-stage-log (cons (list op name) rune-writable-stage-log)) 'writable-answer))) (let ((rune-writable-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-writable-expand-stage) ("RESOLVED" . rune-writable-resolved-stage)))) (list (file-writable-p "MAGIC/leaf") rune-writable-stage-log)))"#,
            r#"(writable-answer ((file-writable-p "/RESOLVED/target") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-writable-inhibit-log nil) (defalias 'rune-writable-inhibit-expand (function (lambda (op name &optional base) (setq rune-writable-inhibit-log (cons op rune-writable-inhibit-log)) "/rune-writable-resolved-absent/target"))) (defalias 'rune-writable-inhibit-final (function (lambda (op name) (setq rune-writable-inhibit-log (cons op rune-writable-inhibit-log)) 'should-not-run))) (let ((rune-writable-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-writable-inhibit-expand) ("rune-writable-resolved-absent" . rune-writable-inhibit-final))) (inhibit-file-name-handlers '(rune-writable-inhibit-expand rune-writable-inhibit-final)) (inhibit-file-name-operation 'file-writable-p)) (list (file-writable-p "MAGIC") rune-writable-inhibit-log)))"#,
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-writable-only-log nil) (defalias 'rune-writable-only-handler (function (lambda (op name &optional base) (setq rune-writable-only-log (cons (list op name) rune-writable-only-log)) 'writable-answer))) (let ((rune-writable-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-writable-only-handler))) (inhibit-file-name-handlers '(rune-writable-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-writable-p "/MAGIC/original") rune-writable-only-log)))"#,
            r#"(writable-answer ((file-writable-p "/MAGIC/original")))"#,
        );
    }

    #[test]
    fn expansion_errors_stop_before_writability_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-writable-error-stage-log nil) (defalias 'rune-writable-invalid-expand (function (lambda (op name &optional base) (setq rune-writable-error-stage-log (cons op rune-writable-error-stage-log)) 3))) (let ((rune-writable-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-writable-invalid-expand)))) (list (condition-case err (file-writable-p "MAGIC") (error err)) rune-writable-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_creatable_lookup_without_creating_or_rewriting() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("rune-writable-expand-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> anyhow::Result<()> {
            let present = dir.join("present"); let missing = dir.join("missing");
            std::fs::write(&present, "kept")?;
            let source = format!(r#"(progn (defalias 'rune-writable-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/present") {:?} {:?})))) (let ((file-name-handler-alist '(("SOURCE" . rune-writable-redirect)))) (list (file-writable-p "SOURCE/present") (file-writable-p "SOURCE/missing"))))"#, present.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(t t)");
            assert_eq!(std::fs::read_to_string(&present)?, "kept");
            assert!(!missing.exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}




#[cfg(all(test, target_os = "linux"))]
mod accessible_expansion_dispatch_tests {
    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-accessible-stage-log nil) (defalias 'rune-accessible-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-accessible-stage-log (cons (list op name base) rune-accessible-stage-log)) "/RESOLVED/target"))) (defalias 'rune-accessible-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-accessible-stage-log (cons (list op name) rune-accessible-stage-log)) 'accessible-answer))) (let ((rune-accessible-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-accessible-expand-stage) ("RESOLVED" . rune-accessible-resolved-stage)))) (list (file-accessible-directory-p "MAGIC/leaf") rune-accessible-stage-log)))"#,
            r#"(accessible-answer ((file-accessible-directory-p "/RESOLVED/target") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-accessible-inhibit-log nil) (defalias 'rune-accessible-inhibit-expand (function (lambda (op name &optional base) (setq rune-accessible-inhibit-log (cons op rune-accessible-inhibit-log)) "/rune-accessible-resolved-absent/target"))) (defalias 'rune-accessible-inhibit-final (function (lambda (op name) (setq rune-accessible-inhibit-log (cons op rune-accessible-inhibit-log)) 'should-not-run))) (let ((rune-accessible-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-accessible-inhibit-expand) ("rune-accessible-resolved-absent" . rune-accessible-inhibit-final))) (inhibit-file-name-handlers '(rune-accessible-inhibit-expand rune-accessible-inhibit-final)) (inhibit-file-name-operation 'file-accessible-directory-p)) (list (file-accessible-directory-p "MAGIC") rune-accessible-inhibit-log)))"#,
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-accessible-only-log nil) (defalias 'rune-accessible-only-handler (function (lambda (op name &optional base) (setq rune-accessible-only-log (cons (list op name) rune-accessible-only-log)) 'accessible-answer))) (let ((rune-accessible-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-accessible-only-handler))) (inhibit-file-name-handlers '(rune-accessible-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-accessible-directory-p "/MAGIC/original") rune-accessible-only-log)))"#,
            r#"(accessible-answer ((file-accessible-directory-p "/MAGIC/original")))"#,
        );
    }

    #[test]
    fn expansion_errors_stop_before_directory_access_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-accessible-error-stage-log nil) (defalias 'rune-accessible-invalid-expand (function (lambda (op name &optional base) (setq rune-accessible-error-stage-log (cons op rune-accessible-error-stage-log)) 3))) (let ((rune-accessible-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-accessible-invalid-expand)))) (list (condition-case err (file-accessible-directory-p "MAGIC") (error err)) rune-accessible-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_searchable_directory_not_regular_file() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("rune-accessible-expand-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir(&dir)?;
        let result = (|| -> anyhow::Result<()> {
            let child = dir.join("child"); let ordinary = dir.join("ordinary"); let missing = dir.join("missing");
            std::fs::create_dir(&child)?;
            std::fs::write(&ordinary, "kept")?;
            let source = format!(r#"(progn (defalias 'rune-accessible-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/child") {:?} (if (equal name "SOURCE/file") {:?} {:?}))))) (let ((file-name-handler-alist '(("SOURCE" . rune-accessible-redirect)))) (list (file-accessible-directory-p "SOURCE/child") (file-accessible-directory-p "SOURCE/file") (file-accessible-directory-p "SOURCE/missing"))))"#, child.to_str().unwrap(), ordinary.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(t nil nil)");
            assert_eq!(std::fs::read_to_string(&ordinary)?, "kept");
            assert!(!missing.exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&dir)?; result
    }
}


#[cfg(all(test, unix))]
mod directory_expansion_dispatch_tests {
    pub(super) struct DirectoryFixture(pub(super) std::path::PathBuf);

    impl DirectoryFixture {
        pub(super) fn new() -> std::io::Result<Self> {
            use std::os::unix::fs::DirBuilderExt;
            let path = std::env::temp_dir().join(format!(
                "rune-directory-fixture-{}-{}", std::process::id(), rand::random::<u64>()
            ));
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for DirectoryFixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-directory-stage-log nil) (defalias 'rune-directory-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-directory-stage-log (cons (list op name base) rune-directory-stage-log)) "/RESOLVED/target///"))) (defalias 'rune-directory-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-directory-stage-log (cons (list op name) rune-directory-stage-log)) (if (eq op 'directory-file-name) "/RESOLVED/target" 'directory-answer)))) (let ((rune-directory-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-directory-expand-stage) ("RESOLVED" . rune-directory-resolved-stage)))) (list (file-directory-p "MAGIC/leaf") rune-directory-stage-log)))"#,
            r#"(directory-answer ((file-directory-p "/RESOLVED/target") (directory-file-name "/RESOLVED/target///") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() -> anyhow::Result<()> {
        let fixture = DirectoryFixture::new()?;
        let missing = fixture.0.join("rune-directory-resolved-absent");
        assert!(!missing.exists());
        crate::interpreter::assert_lisp(
            &format!(r#"(progn (defvar rune-directory-inhibit-log nil) (defalias 'rune-directory-inhibit-expand (function (lambda (op name &optional base) (setq rune-directory-inhibit-log (cons op rune-directory-inhibit-log)) {missing:?}))) (defalias 'rune-directory-inhibit-final (function (lambda (op name) (setq rune-directory-inhibit-log (cons op rune-directory-inhibit-log)) 'should-not-run))) (let ((rune-directory-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-directory-inhibit-expand) ("rune-directory-resolved-absent" . rune-directory-inhibit-final))) (inhibit-file-name-handlers '(rune-directory-inhibit-expand rune-directory-inhibit-final)) (inhibit-file-name-operation 'file-directory-p)) (list (file-directory-p "MAGIC") rune-directory-inhibit-log)))"#, missing = missing.to_str().unwrap()),
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-directory-only-log nil) (defalias 'rune-directory-only-handler (function (lambda (op name &optional base) (setq rune-directory-only-log (cons (list op name) rune-directory-only-log)) 'directory-answer))) (let ((rune-directory-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-directory-only-handler))) (inhibit-file-name-handlers '(rune-directory-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-directory-p "/MAGIC/original") rune-directory-only-log)))"#,
            r#"(directory-answer ((file-directory-p "/MAGIC/original")))"#,
        );
        assert!(!missing.exists());
        Ok(())
    }

    #[test]
    fn expansion_errors_stop_before_directory_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-directory-error-stage-log nil) (defalias 'rune-directory-invalid-expand (function (lambda (op name &optional base) (setq rune-directory-error-stage-log (cons op rune-directory-error-stage-log)) 3))) (let ((rune-directory-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-directory-invalid-expand)))) (list (condition-case err (file-directory-p "MAGIC") (error err)) rune-directory-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_directory_not_regular_file() -> anyhow::Result<()> {
        let fixture = DirectoryFixture::new()?;
        let dir = &fixture.0;
        let result = (|| -> anyhow::Result<()> {
            let child = dir.join("child"); let ordinary = dir.join("ordinary"); let missing = dir.join("missing");
            let link = dir.join("link");
            let dangling = dir.join("dangling");
            std::fs::create_dir(&child)?;
            std::fs::write(&ordinary, "kept")?;
            std::os::unix::fs::symlink(&child, &link)?;
            std::os::unix::fs::symlink(&missing, &dangling)?;
            let source = format!(r#"(progn (defalias 'rune-directory-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/child") {:?} (if (equal name "SOURCE/file") {:?} (if (equal name "SOURCE/link") {:?} (if (equal name "SOURCE/dangling") {:?} {:?}))))))) (let ((file-name-handler-alist '(("SOURCE" . rune-directory-redirect)))) (list (file-directory-p "SOURCE/child") (file-directory-p "SOURCE/file") (file-directory-p "SOURCE/missing") (file-directory-p "SOURCE/link") (file-directory-p "SOURCE/dangling"))))"#, child.to_str().unwrap(), ordinary.to_str().unwrap(), link.to_str().unwrap(), dangling.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(t nil nil t nil)");
            assert_eq!(std::fs::read_to_string(&ordinary)?, "kept");
            assert!(!missing.exists());
            assert_eq!(std::fs::read_link(&link)?, child);
            assert_eq!(std::fs::read_link(&dangling)?, missing);
            Ok(())
        })();
        result
    }
}


#[cfg(all(test, unix))]
mod regular_expansion_dispatch_tests {
    #[test]
    fn expansion_precedes_resolved_operation_once_across_gc() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-regular-stage-log nil) (defalias 'rune-regular-expand-stage (function (lambda (op name &optional base) (garbage-collect) (setq rune-regular-stage-log (cons (list op name base) rune-regular-stage-log)) "/RESOLVED/target///"))) (defalias 'rune-regular-resolved-stage (function (lambda (op name) (garbage-collect) (setq rune-regular-stage-log (cons (list op name) rune-regular-stage-log)) (if (eq op 'directory-file-name) "/RESOLVED/target" 'regular-answer)))) (let ((rune-regular-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-regular-expand-stage) ("RESOLVED" . rune-regular-resolved-stage)))) (list (file-regular-p "MAGIC/leaf") rune-regular-stage-log)))"#,
            r#"(regular-answer ((file-regular-p "/RESOLVED/target") (directory-file-name "/RESOLVED/target///") (expand-file-name "MAGIC/leaf" nil)))"#,
        );
    }

    #[test]
    fn operation_inhibition_does_not_suppress_expansion_stage() -> anyhow::Result<()> {
        let fixture = super::directory_expansion_dispatch_tests::DirectoryFixture::new()?;
        let missing = fixture.0.join("rune-regular-resolved-absent");
        assert!(!missing.exists());
        crate::interpreter::assert_lisp(
            &format!(r#"(progn (defvar rune-regular-inhibit-log nil) (defalias 'rune-regular-inhibit-expand (function (lambda (op name &optional base) (setq rune-regular-inhibit-log (cons op rune-regular-inhibit-log)) {missing:?}))) (defalias 'rune-regular-inhibit-final (function (lambda (op name) (setq rune-regular-inhibit-log (cons op rune-regular-inhibit-log)) 'should-not-run))) (let ((rune-regular-inhibit-log nil) (file-name-handler-alist '(("MAGIC" . rune-regular-inhibit-expand) ("rune-regular-resolved-absent" . rune-regular-inhibit-final))) (inhibit-file-name-handlers '(rune-regular-inhibit-expand rune-regular-inhibit-final)) (inhibit-file-name-operation 'file-regular-p)) (list (file-regular-p "MAGIC") rune-regular-inhibit-log)))"#, missing = missing.to_str().unwrap()),
            "(nil (expand-file-name))",
        );
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-regular-only-log nil) (defalias 'rune-regular-only-handler (function (lambda (op name &optional base) (setq rune-regular-only-log (cons (list op name) rune-regular-only-log)) 'regular-answer))) (let ((rune-regular-only-log nil) (file-name-handler-alist '(("MAGIC" . rune-regular-only-handler))) (inhibit-file-name-handlers '(rune-regular-only-handler)) (inhibit-file-name-operation 'expand-file-name)) (list (file-regular-p "/MAGIC/original") rune-regular-only-log)))"#,
            r#"(regular-answer ((file-regular-p "/MAGIC/original")))"#,
        );
        assert!(!missing.exists());
        Ok(())
    }

    #[test]
    fn expansion_errors_stop_before_regular_handler() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-regular-error-stage-log nil) (defalias 'rune-regular-invalid-expand (function (lambda (op name &optional base) (setq rune-regular-error-stage-log (cons op rune-regular-error-stage-log)) 3))) (let ((rune-regular-error-stage-log nil) (file-name-handler-alist '(("MAGIC" . rune-regular-invalid-expand)))) (list (condition-case err (file-regular-p "MAGIC") (error err)) rune-regular-error-stage-log)))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (expand-file-name))"#,
        );
    }

    #[test]
    fn expansion_redirects_regular_file_not_directory() -> anyhow::Result<()> {
        let fixture = super::directory_expansion_dispatch_tests::DirectoryFixture::new()?;
        let dir = &fixture.0;
        let result = (|| -> anyhow::Result<()> {
            let child = dir.join("child"); let ordinary = dir.join("ordinary"); let missing = dir.join("missing");
            let link = dir.join("link");
            let dangling = dir.join("dangling");
            std::fs::create_dir(&child)?;
            std::fs::write(&ordinary, "kept")?;
            std::os::unix::fs::symlink(&ordinary, &link)?;
            std::os::unix::fs::symlink(&missing, &dangling)?;
            let source = format!(r#"(progn (defalias 'rune-regular-redirect (function (lambda (op name &optional base) (garbage-collect) (if (equal name "SOURCE/child") {:?} (if (equal name "SOURCE/file") {:?} (if (equal name "SOURCE/link") {:?} (if (equal name "SOURCE/dangling") {:?} {:?}))))))) (let ((file-name-handler-alist '(("SOURCE" . rune-regular-redirect)))) (list (file-regular-p "SOURCE/child") (file-regular-p "SOURCE/file") (file-regular-p "SOURCE/missing") (file-regular-p "SOURCE/link") (file-regular-p "SOURCE/dangling"))))"#, child.to_str().unwrap(), ordinary.to_str().unwrap(), link.to_str().unwrap(), dangling.to_str().unwrap(), missing.to_str().unwrap());
            crate::interpreter::assert_lisp(&source, "(nil t nil t nil)");
            assert_eq!(std::fs::read_to_string(&ordinary)?, "kept");
            assert!(!missing.exists());
            assert_eq!(std::fs::read_link(&link)?, ordinary);
            assert_eq!(std::fs::read_link(&dangling)?, missing);
            Ok(())
        })();
        result
    }
}

#[cfg(all(test, unix))]
mod directory_file_name_tests {
    #[test]
    fn original_name_and_handler_result_survive_gc_without_reprocessing() {
        crate::interpreter::assert_lisp(
            r#"(progn
              (defvar rune-directory-conversion-log nil)
              (defalias 'rune-directory-conversion-handler
                (function (lambda (operation name)
                  (garbage-collect)
                  (setq rune-directory-conversion-log (list operation name))
                  "/returned///")))
              (let ((rune-directory-conversion-log nil)
                    (file-name-handler-alist '(("MAGIC" . rune-directory-conversion-handler))))
                (list (directory-file-name "MAGIC///") rune-directory-conversion-log)))"#,
            r#"("/returned///" (directory-file-name "MAGIC///"))"#,
        );
    }

    #[test]
    fn operation_inhibition_allows_recursive_lexical_conversion() {
        crate::interpreter::assert_lisp(
            r#"(progn
              (defvar rune-directory-conversion-count 0)
              (defalias 'rune-directory-conversion-recursive
                (function (lambda (operation name)
                  (setq rune-directory-conversion-count (1+ rune-directory-conversion-count))
                  (let ((inhibit-file-name-handlers '(rune-directory-conversion-recursive))
                        (inhibit-file-name-operation 'directory-file-name))
                    (directory-file-name name)))))
              (let ((rune-directory-conversion-count 0)
                    (file-name-handler-alist '(("MAGIC" . rune-directory-conversion-recursive)))
                    (inhibit-file-name-handlers '(rune-directory-conversion-recursive))
                    (inhibit-file-name-operation 'file-directory-p))
                (list (directory-file-name "MAGIC///") rune-directory-conversion-count)))"#,
            r#"("MAGIC" 1)"#,
        );
    }

    #[test]
    fn invalid_handler_results_and_argument_types_signal() {
        crate::interpreter::assert_lisp(
            r#"(progn
              (defalias 'rune-directory-conversion-invalid (function (lambda (operation name) 3)))
              (let ((file-name-handler-alist '(("MAGIC" . rune-directory-conversion-invalid))))
                (list (condition-case err (directory-file-name "MAGIC/") (error err))
                      (condition-case err (directory-file-name 3) (wrong-type-argument err)))))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (wrong-type-argument stringp 3))"#,
        );
    }

    #[test]
    fn utf8_embedded_nul_is_lexically_preserved() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let directory = cx.add("é\0///");
        root!(directory, cx);
        let result = directory_file_name(directory, env, cx)?;
        let ObjectType::String(text) = result.untag() else {
            anyhow::bail!("directory-file-name changed UTF-8 representation");
        };
        assert_eq!(&**text, "é\0");
        Ok(())
    }

    #[test]
    fn handler_returned_raw_bytes_survive_gc_without_reprocessing() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        env.set_var(sym::DEFAULT_DIRECTORY, cx.add(b"raw-\xff\0///".to_vec()))?;
        crate::lread::load_internal(
            r#"(progn
              (defalias 'rune-directory-conversion-raw
                (function (lambda (operation name) (garbage-collect) default-directory)))
              (setq file-name-handler-alist '(("MAGIC" . rune-directory-conversion-raw))))"#,
            cx, env,
        )?;
        let directory = cx.add("MAGIC///");
        root!(directory, cx);
        let result = directory_file_name(directory, env, cx)?;
        let ObjectType::ByteString(bytes) = result.untag() else {
            anyhow::bail!("directory-file-name changed handler-returned raw representation");
        };
        assert_eq!(&**bytes, b"raw-\xff\0///");
        Ok(())
    }

    #[test]
    fn raw_bytes_are_preserved_without_handlers() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        for (input, expected) in [
            (&b""[..], &b""[..]), (&b"/"[..], &b"/"[..]),
            (&b"//"[..], &b"//"[..]), (&b"///"[..], &b"/"[..]),
            (&b"raw-\xff///"[..], &b"raw-\xff"[..]),
            (&b"nul\0///"[..], &b"nul\0"[..]),
        ] {
            let directory = cx.add(input.to_vec());
            root!(directory, cx);
            let result = directory_file_name(directory, env, cx)?;
            let ObjectType::ByteString(bytes) = result.untag() else {
                anyhow::bail!("directory-file-name changed raw string representation");
            };
            assert_eq!(&**bytes, expected);
        }
        let directory = cx.add(b"raw-\xff/".to_vec());
        root!(directory, cx);
        env.set_var(sym::FILE_NAME_HANDLER_ALIST, rune_core::macros::list![NIL; cx])?;
        assert!(directory_file_name(directory, env, cx).is_err());
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod predicate_conversion_dispatch_tests {
    #[test]
    fn conversion_redirects_both_predicates_once_across_gc() {
        for predicate in ["file-directory-p", "file-regular-p"] {
            crate::interpreter::assert_lisp(
                &format!(r#"(progn
                  (defvar rune-conversion-pipeline-log nil)
                  (defalias 'rune-conversion-pipeline-handler
                    (function (lambda (op name &optional base)
                      (garbage-collect)
                      (setq rune-conversion-pipeline-log (cons (list op name) rune-conversion-pipeline-log))
                      (if (eq op 'expand-file-name) "/MAGIC/source///"
                        (if (eq op 'directory-file-name) "/MAGIC/returned///" 'answer)))))
                  (let ((file-name-handler-alist '(("MAGIC" . rune-conversion-pipeline-handler)))
                        (rune-conversion-pipeline-log nil))
                    (list ({predicate} "MAGIC/input") rune-conversion-pipeline-log)))"#),
                &format!(r#"(answer (({predicate} "/MAGIC/returned///") (directory-file-name "/MAGIC/source///") (expand-file-name "MAGIC/input")))"#),
            );
        }
    }

    #[test]
    fn conversion_inhibition_is_per_operation_and_double_slash_is_dispatched() {
        for predicate in ["file-directory-p", "file-regular-p"] {
            crate::interpreter::assert_lisp(
                &format!(r#"(progn
                  (defvar rune-conversion-root-log nil)
                  (defalias 'rune-conversion-root-handler
                    (function (lambda (op name &optional base)
                      (setq rune-conversion-root-log (cons (list op name) rune-conversion-root-log))
                      (if (eq op 'expand-file-name) name
                        (if (eq op 'directory-file-name) "/converted" 'answer)))))
                  (let ((file-name-handler-alist '(("/" . rune-conversion-root-handler)))
                        (inhibit-file-name-handlers '(rune-conversion-root-handler))
                        (inhibit-file-name-operation 'directory-file-name)
                        (rune-conversion-root-log nil))
                    (list ({predicate} "//") rune-conversion-root-log
                      (let ((inhibit-file-name-operation 'file-exists-p)
                            (rune-conversion-root-log nil))
                        (list ({predicate} "//") rune-conversion-root-log)))))"#),
                &format!(r#"(answer (({predicate} "//") (expand-file-name "//")) (answer (({predicate} "/converted") (directory-file-name "//") (expand-file-name "//"))))"#),
            );
        }
    }

    #[test]
    fn conversion_errors_and_invalid_local_results_stop_final_dispatch() {
        for predicate in ["file-directory-p", "file-regular-p"] {
            for result in ["3", "(signal 'error '(\"conversion-error\"))", "(concat \"/MAGIC/\" (make-string 1 0))", "(make-string 131074 47)"] {
                crate::interpreter::assert_lisp(
                    &format!(r#"(progn
                      (defvar rune-conversion-error-log nil)
                      (defalias 'rune-conversion-error-handler
                        (function (lambda (op name &optional base)
                          (setq rune-conversion-error-log (cons op rune-conversion-error-log))
                          (if (eq op 'expand-file-name) "/MAGIC/source/"
                            (if (eq op 'directory-file-name) {result} 'unexpected)))))
                      (let ((file-name-handler-alist '(("MAGIC" . rune-conversion-error-handler)))
                            (rune-conversion-error-log nil))
                        (list (condition-case nil ({predicate} "MAGIC/input") (error 'caught))
                              rune-conversion-error-log)))"#),
                    "(caught (directory-file-name expand-file-name))",
                );
            }
        }
    }

    #[test]
    fn expanded_conversion_keeps_existing_path_budget() {
        for predicate in ["file-directory-p", "file-regular-p"] {
            for length in [65536, 65537, 131073, 131074] {
                let expected = if length <= 131073 {
                    format!("(answer ({predicate} directory-file-name expand-file-name))")
                } else {
                    "(caught (expand-file-name))".to_owned()
                };
                crate::interpreter::assert_lisp(
                    &format!(r#"(progn
                      (defvar rune-conversion-budget-log nil)
                      (defalias 'rune-conversion-budget-handler
                        (function (lambda (op name &optional base)
                          (garbage-collect)
                          (setq rune-conversion-budget-log (cons op rune-conversion-budget-log))
                          (if (eq op 'expand-file-name) (make-string {length} 47)
                            (if (eq op 'directory-file-name) "/short" 'answer)))))
                      (let ((file-name-handler-alist '(("/" . rune-conversion-budget-handler)))
                            (rune-conversion-budget-log nil))
                        (list (condition-case nil ({predicate} "/input") (error 'caught))
                              rune-conversion-budget-log)))"#),
                    &expected,
                );
            }
        }
    }

    #[test]
    fn conversion_redirects_to_isolated_filesystem_targets() -> anyhow::Result<()> {
        let fixture = super::directory_expansion_dispatch_tests::DirectoryFixture::new()?;
        let child = fixture.0.join("child");
        let ordinary = fixture.0.join("ordinary");
        let missing = fixture.0.join("missing");
        std::fs::create_dir(&child)?;
        std::fs::write(&ordinary, "kept")?;
        assert!(!missing.exists());
        for (target, expected) in [(&child, "(t nil)"), (&ordinary, "(nil t)"), (&missing, "(nil nil)")] {
            crate::interpreter::assert_lisp(
                &format!(r#"(progn
                  (defalias 'rune-conversion-local-handler
                    (function (lambda (op name &optional base)
                      (garbage-collect)
                      (if (eq op 'expand-file-name) "/MAGIC/target/" {:?}))))
                  (let ((file-name-handler-alist '(("MAGIC" . rune-conversion-local-handler))))
                    (list (file-directory-p "MAGIC") (file-regular-p "MAGIC"))))"#, target.to_str().unwrap()),
                expected,
            );
        }
        assert_eq!(std::fs::read_to_string(&ordinary)?, "kept");
        assert!(!missing.exists());
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod file_name_as_directory_tests {
    #[test]
    fn original_name_and_handler_result_survive_gc_without_reprocessing() {
        crate::interpreter::assert_lisp(
            r#"(progn
              (defvar rune-as-directory-conversion-log nil)
              (defalias 'rune-as-directory-conversion-handler
                (function (lambda (operation name)
                  (garbage-collect)
                  (setq rune-as-directory-conversion-log (list operation name))
                  "/returned")))
              (let ((rune-as-directory-conversion-log nil)
                    (file-name-handler-alist '(("MAGIC" . rune-as-directory-conversion-handler))))
                (list (file-name-as-directory "MAGIC///") rune-as-directory-conversion-log)))"#,
            r#"("/returned" (file-name-as-directory "MAGIC///"))"#,
        );
    }

    #[test]
    fn operation_inhibition_allows_recursive_lexical_conversion() {
        crate::interpreter::assert_lisp(
            r#"(progn
              (defvar rune-as-directory-conversion-count 0)
              (defalias 'rune-as-directory-conversion-recursive
                (function (lambda (operation name)
                  (setq rune-as-directory-conversion-count (1+ rune-as-directory-conversion-count))
                  (let ((inhibit-file-name-handlers '(rune-as-directory-conversion-recursive))
                        (inhibit-file-name-operation 'file-name-as-directory))
                    (file-name-as-directory name)))))
              (let ((rune-as-directory-conversion-count 0)
                    (file-name-handler-alist '(("MAGIC" . rune-as-directory-conversion-recursive)))
                    (inhibit-file-name-handlers '(rune-as-directory-conversion-recursive))
                    (inhibit-file-name-operation 'file-directory-p))
                (list (file-name-as-directory "MAGIC///") rune-as-directory-conversion-count)))"#,
            r#"("MAGIC///" 1)"#,
        );
    }

    #[test]
    fn invalid_handler_results_and_argument_types_signal() {
        crate::interpreter::assert_lisp(
            r#"(progn
              (defalias 'rune-as-directory-conversion-invalid (function (lambda (operation name) 3)))
              (let ((file-name-handler-alist '(("MAGIC" . rune-as-directory-conversion-invalid))))
                (list (condition-case err (file-name-as-directory "MAGIC/") (error err))
                      (condition-case err (file-name-as-directory 3) (wrong-type-argument err)))))"#,
            r#"((error "Invalid handler in `file-name-handler-alist'") (wrong-type-argument stringp 3))"#,
        );
        crate::interpreter::assert_lisp(
            r#"(let ((file-name-handler-alist nil))
                 (condition-case err (file-name-as-directory 3) (wrong-type-argument err)))"#,
            "(wrong-type-argument stringp 3)",
        );
    }

    #[test]
    fn utf8_embedded_nul_is_lexically_preserved() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let directory = cx.add("é\0");
        root!(directory, cx);
        let result = file_name_as_directory(directory, env, cx)?;
        let ObjectType::String(text) = result.untag() else {
            anyhow::bail!("file-name-as-directory changed UTF-8 representation");
        };
        assert_eq!(&**text, "é\0/");
        Ok(())
    }

    #[test]
    fn handler_returned_raw_bytes_survive_gc_without_reprocessing() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        env.set_var(sym::DEFAULT_DIRECTORY, cx.add(b"raw-\xff\0".to_vec()))?;
        crate::lread::load_internal(
            r#"(progn
              (defalias 'rune-as-directory-conversion-raw
                (function (lambda (operation name) (garbage-collect) default-directory)))
              (setq file-name-handler-alist '(("MAGIC" . rune-as-directory-conversion-raw))))"#,
            cx, env,
        )?;
        let directory = cx.add("MAGIC///");
        root!(directory, cx);
        let result = file_name_as_directory(directory, env, cx)?;
        let ObjectType::ByteString(bytes) = result.untag() else {
            anyhow::bail!("file-name-as-directory changed handler-returned raw representation");
        };
        assert_eq!(&**bytes, b"raw-\xff\0");
        Ok(())
    }

    #[test]
    fn lexical_conversion_preserves_large_input_and_idempotence() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        for raw in [false, true] {
            let name = if raw { cx.add(vec![b'x'; 65536]) } else { cx.add("x".repeat(65536)) };
            root!(name, cx);
            let result = file_name_as_directory(name, env, cx)?;
            match result.untag() {
                ObjectType::ByteString(bytes) if raw => {
                    assert_eq!(bytes.len(), 65537);
                    assert_eq!(bytes.last(), Some(&b'/'));
                }
                ObjectType::String(text) if !raw => {
                    assert_eq!(text.len(), 65537);
                    assert!(text.ends_with('/'));
                }
                _ => anyhow::bail!("file-name-as-directory changed representation"),
            }
            root!(result, cx);
            let again = file_name_as_directory(result, env, cx)?;
            match again.untag() {
                ObjectType::ByteString(bytes) if raw => assert_eq!(bytes.len(), 65537),
                ObjectType::String(text) if !raw => assert_eq!(text.len(), 65537),
                _ => anyhow::bail!("repeated conversion changed representation"),
            }
            let name = if raw { cx.add(vec![b'x'; 65537]) } else { cx.add("x".repeat(65537)) };
            root!(name, cx);
            let longer = file_name_as_directory(name, env, cx)?;
            match longer.untag() {
                ObjectType::ByteString(bytes) if raw => assert_eq!(bytes.len(), 65538),
                ObjectType::String(text) if !raw => assert_eq!(text.len(), 65538),
                _ => anyhow::bail!("large conversion changed representation"),
            }
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, rune_core::macros::list![NIL; cx])?;
            assert!(file_name_as_directory(name, env, cx).is_err());
            env.set_var(sym::FILE_NAME_HANDLER_ALIST, NIL.into())?;
        }
        Ok(())
    }

    #[test]
    fn raw_bytes_are_preserved_without_handlers() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        for (input, expected) in [
            (&b""[..], &b"./"[..]), (&b"/"[..], &b"/"[..]),
            (&b"//"[..], &b"//"[..]), (&b"///"[..], &b"///"[..]),
            (&b"raw-\xff"[..], &b"raw-\xff/"[..]),
            (&b"nul\0"[..], &b"nul\0/"[..]),
        ] {
            let directory = cx.add(input.to_vec());
            root!(directory, cx);
            let result = file_name_as_directory(directory, env, cx)?;
            let ObjectType::ByteString(bytes) = result.untag() else {
                anyhow::bail!("file-name-as-directory changed raw string representation");
            };
            assert_eq!(&**bytes, expected);
        }
        let directory = cx.add(b"raw-\xff/".to_vec());
        root!(directory, cx);
        env.set_var(sym::FILE_NAME_HANDLER_ALIST, rune_core::macros::list![NIL; cx])?;
        assert!(file_name_as_directory(directory, env, cx).is_err());
        Ok(())
    }
}
