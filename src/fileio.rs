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
    let file_name_handler_alist = symbol_value(FILE_NAME_HANDLER_ALIST, env, cx)?.as_list().ok()?;
    let inhibit_file_name_handlers =
        symbol_value(INHIBIT_FILE_NAME_HANDLERS, env, cx).and_then(|sym| sym.as_list().ok());

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

defsym!(EXCL);
#[defun]
#[expect(clippy::too_many_arguments)]
fn write_region(
    start: i64,
    end: i64,
    filename: &str,
    append: Option<Object>,
    visit: OptionalFlag,
    lockname: OptionalFlag,
    mustbenew: Option<Object>,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<()> {
    use std::io::{Seek, SeekFrom, Write};
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
    // Capture and validate an immutable owner root before opening/truncating.
    let start = usize::try_from(start)?;
    let end = usize::try_from(end)?;
    let (start, end) = (start.min(end), start.max(end));
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    ensure!(start > 0 && end <= text.len_chars() + 1, "Buffer range out of bounds");
    let bytes = text.char_to_byte(rune_text::CharPos(start - 1))?
        ..text.char_to_byte(rune_text::CharPos(end - 1))?;
    let chunks = text.chunks(bytes)?;
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
    for chunk in chunks {
        file.write_all(chunk.as_bytes())?;
    }
    Ok(())
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
        set(FILE_NAME_HANDLER_ALIST, file_name_handler_alist, env).unwrap();

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
        set(FILE_NAME_HANDLER_ALIST, file_name_handler_alist, env).unwrap();
        set(INHIBIT_FILE_NAME_HANDLERS, list![inhibit_handler; cx], env).unwrap();

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

        set(FILE_NAME_HANDLER_ALIST, list![Cons::new(NIL, handler, cx); cx], env).unwrap();
        find_file_name_handler("example", NIL, env, cx);

        set(FILE_NAME_HANDLER_ALIST, list![Cons::new(".*", NIL , cx); cx], env).unwrap();
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
