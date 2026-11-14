//! Scoped local executable access, using effective credentials, not mode-bit guesses.
use crate::core::{env::{Env, sym}, gc::{Context, Rt}, object::{Object, ObjectType}};
use anyhow::{Result, ensure};
use rune_macros::defun;
use rune_core::macros::list;
use std::path::{Path, PathBuf};

const MAX_PATH_BYTES: usize = 65536;
defsym!(FILENAMEP);
fn path_type_error(predicate: crate::core::object::Symbol, value: Object, env: &mut Rt<Env>, cx: &Context) -> anyhow::Error {
    env.initialize_condition_metadata(cx);
    let data = rune_core::macros::list!(predicate, value; cx);
    crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into()
}
fn local_path(value: Object, env: &mut Rt<Env>, cx: &Context) -> Result<PathBuf> {
    // Inspect borrowed storage before any native path allocation or recursive
    // formatting. The owner argument remains rooted, and shared cx excludes GC.
    let bytes: &[u8] = match value.untag() {
        ObjectType::String(s) => s.as_bytes(),
        ObjectType::ByteString(s) => &s,
        _ => return Err(path_type_error(sym::STRINGP, value, env, cx)),
    };
    ensure!(bytes.len() <= MAX_PATH_BYTES, "File access path exceeds 64 KiB limit");
    if bytes.contains(&0) { return Err(path_type_error(sym::FILENAMEP, value, env, cx)); }
    #[cfg(unix)] { use std::os::unix::ffi::OsStrExt; Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes))) }
    #[cfg(not(unix))] { Ok(PathBuf::from(std::str::from_utf8(bytes)?)) }
}

#[defun]
fn file_executable_p(filename: Object, env: &mut Rt<Env>, cx: &Context) -> Result<bool> {
    let path = local_path(filename, env, cx)?;
    // GNU local empty names denote default-directory. No ambient cwd fallback.
    let path = if path.is_absolute() { path } else {
        let directory = local_path(crate::data::symbol_value(sym::DEFAULT_DIRECTORY, env, cx)?, env, cx)?;
        ensure!(directory.is_absolute(), "Relative default-directory is not supported for file access");
        ensure!(directory.as_os_str().as_encoded_bytes().len().checked_add(path.as_os_str().as_encoded_bytes().len()).and_then(|n| n.checked_add(1)).is_some_and(|n| n <= MAX_PATH_BYTES), "Joined file access path exceeds 64 KiB limit");
        directory.join(path)
    };
    executable_access(&path)
}

fn executable_access(path: &Path) -> Result<bool> {
    #[cfg(target_os = "linux")] {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: name is a live NUL-terminated native path; faccessat does not
        // retain it. AT_EACCESS requests the owner's effective credentials.
        let result = unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::X_OK, libc::AT_EACCESS) };
        Ok(result == 0)
    }
    #[cfg(not(target_os = "linux"))] { let _ = path; anyhow::bail!("Effective executable access is not implemented on this platform") }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn executable_predicate_matches_gnu_local_permissions_and_links() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = loop {
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("rune-executable-oracle-{}-{n}",std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("Cannot create exclusive test directory: {e}"),
            }
        };
        struct Cleanup(PathBuf); impl Drop for Cleanup { fn drop(&mut self) { let _=std::fs::remove_dir_all(&self.0); } }
        let _cleanup=Cleanup(dir.clone());
        let executable=dir.join("executable");let plain=dir.join("plain");
        std::fs::write(&executable,b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&plain,b"not executable\n").unwrap();
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&plain,std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&executable,dir.join("link")).unwrap();symlink(dir.join("missing"),dir.join("dangling")).unwrap();
        for path in [&dir, &executable, &plain, &dir.join("link"), &dir.join("dangling"), &dir.join("missing")] {
            crate::assert_elprop!("(file-executable-p {:?})",path.to_str().unwrap());
        }
        crate::assert_elprop!("(let ((default-directory {:?})) (list (file-executable-p \"executable\") (file-executable-p \"plain\") (file-executable-p \"\")))",format!("{}/",dir.display()));
        assert!(executable_access(&executable).unwrap());assert!(!executable_access(&plain).unwrap());
    }
}

#[cfg(test)]
mod invalid_filename_tests {
    #[test]
    fn cyclic_filename_signals_without_recursive_rendering() {
        crate::assert_elprop!(r#"(condition-case e (file-executable-p "a\0b") (wrong-type-argument (list (car e) (car (cdr e)))))"#);
        crate::assert_elprop!(r#"(let ((v (vector nil))) (aset v 0 v) (condition-case e (file-executable-p v) (wrong-type-argument (list (car e) (car (cdr e)) (eq (car (cdr (cdr e))) v)))))"#);
        crate::assert_elprop!(r#"(let* ((v (vector nil)) (f (make-byte-code 0 (make-string 0 0) v 0))) (aset v 0 f) (condition-case e (file-executable-p f) (wrong-type-argument (list (car e) (car (cdr e)) (eq (car (cdr (cdr e))) f)))))"#);
        crate::assert_elprop!(r#"(let ((v (list nil))) (setcar v v) (condition-case e (file-executable-p v) (wrong-type-argument (list (car e) (car (cdr e)) (eq (car (cdr (cdr e))) v)))))"#);
    }
}

#[cfg(test)]
mod path_budget_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn oversized_paths_reject_before_native_copy_and_join() {
        let roots=RootSet::default(); let cx=&mut Context::new(&roots);
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        let large=cx.add(vec![b'x'; MAX_PATH_BYTES+1]);
        assert!(local_path(large,env,cx).is_err());
        let directory=cx.add(format!("/{}", "x".repeat(MAX_PATH_BYTES-2)));
        env.set_var_context(sym::DEFAULT_DIRECTORY,directory,cx).unwrap();
        assert!(file_executable_p(cx.add("relative"),env,cx).is_err());
    }
}
