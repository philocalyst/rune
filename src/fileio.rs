//! File I/O.
use crate::{
    core::{
        cons::Cons,
        env::{Env, sym},
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

#[defun]
pub(crate) fn expand_file_name(
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

#[defun]
fn car_less_than_car(a: &Cons, b: &Cons) -> Result<bool> {
    let a: Number = a.car().try_into()?;
    let b: Number = b.car().try_into()?;
    Ok(a.val() < b.val())
}

#[defun]
fn file_accessible_directory_p(filename: &str) -> bool {
    let path = Path::new(filename);
    path.exists() && path.is_dir()
}

#[defun]
fn file_name_as_directory(filename: &str) -> String {
    if filename.ends_with(MAIN_SEPARATOR) {
        filename.to_owned()
    } else {
        format!("{filename}{MAIN_SEPARATOR}")
    }
}

#[defun]
fn file_directory_p(filename: &str) -> bool {
    if filename.is_empty() { true } else { Path::new(filename).is_dir() }
}

/// Return dirname sans final path separator, unless the string consists entirely of separators.
#[defun]
fn directory_file_name(dirname: &str) -> &str {
    let path = Path::new(dirname);
    let mut path_components = path.components();
    if path_components.clone().next().is_none() {
        return "";
    }

    if path_components.all(|c| c == Component::RootDir || c == Component::Normal("".as_ref())) {
        return "/";
    }

    dirname.strip_suffix(MAIN_SEPARATOR).unwrap_or(dirname)
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

/// Returns the directory part of `filename`, as a directory name, or nil if filename does not include a directory part.
#[defun]
fn file_name_directory(filename: &str) -> Option<String> {
    // TODO: GNU Emacs docs stipulate that "On MS-DOS [ed: presumably windows,
    // too] it can also end in a colon."
    if !filename.contains(MAIN_SEPARATOR) {
        return None;
    }

    if filename.ends_with(MAIN_SEPARATOR) {
        return Some(filename.into());
    }

    let path = Path::new(filename);
    let parent = path.parent()?;

    // Special case for root path so we don't end up returning '//'
    if parent.parent().is_none() {
        return Some(format!("{MAIN_SEPARATOR}"));
    }
    let parent_path = parent.to_str()?;
    Some(format!("{parent_path}{MAIN_SEPARATOR}"))
}

/// Returns the non-directory part of `filename`
#[defun]
fn file_name_nondirectory(filename: &str) -> &str {
    if filename.ends_with(MAIN_SEPARATOR) {
        return "";
    }

    let path = Path::new(filename);
    let Some(file_name) = path.file_name() else {
        return "";
    };

    file_name.to_str().unwrap()
}

/// Return non-nil if NAME ends with a directory separator character.
#[defun]
fn directory_name_p(name: &str) -> bool {
    name.ends_with(MAIN_SEPARATOR)
}

defvar!(FILE_NAME_HANDLER_ALIST);
defvar!(INHIBIT_FILE_NAME_HANDLERS);
defsym!(INHIBIT_FILE_NAME_OPERATION);

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

    file_name_handler_alist
        .filter_map(|elem| {
            let cons = elem.ok()?.cons()?;
            let regexp = cons.car().string()?;
            let re = Regex::new(&lisp_regex_to_rust(regexp))
                .unwrap_or_else(|err| panic!("Invalid regexp '{regexp}': {err}"));
            if re.is_match(filename).ok()? {
                let handler = cons.cdr().symbol()?;
                if operation != sym::INHIBIT_FILE_NAME_OPERATION
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
fn file_symlink_p(filename: &str) -> bool {
    Path::new(filename).is_symlink()
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

/// Concatenate components to directory, inserting path separators as required.
#[defun]
fn file_name_concat(directory: &str, rest_components: &[Object]) -> Result<String> {
    let mut path = String::from(directory);

    // All components must be stringp...
    for r_c in rest_components {
        let ObjectType::String(s) = r_c.untag() else {
            bail!(TypeError::new(Type::String, r_c));
        };

        // Append separator before adding the new element, but only if the
        // existing path isn't already terminated with a "/"
        if !path.ends_with(MAIN_SEPARATOR) {
            path.push(MAIN_SEPARATOR)
        }

        path.push_str(s.as_ref());
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

        let result = find_file_name_handler("test.txt", NIL, env, cx);
        assert_eq!(result, inhibit_handler);

        let result = find_file_name_handler("test.txt", sym::INHIBIT_FILE_NAME_OPERATION, env, cx);
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
