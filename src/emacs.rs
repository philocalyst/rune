//! The Emacs environment and runtime.
use rune_macros::defun;

#[defun]
fn kill_emacs() {}

defvar!(EMACS_VERSION, "27.1");
defvar!(SYSTEM_TYPE, "darwin");
defvar!(DUMP_MODE);
defvar!(COMMAND_LINE_ARGS, list![""]);
defvar!(DEFAULT_DIRECTORY, env.startup_default_directory.clone());
defvar_bool!(NONINTERACTIVE, true);
defvar!(AFTER_INIT_TIME);

// Read the native environment only for rooted variable initialization. Later
// lookups and child snapshots use the Lisp variable, never an OS fallback.
pub(crate) fn initial_process_environment<'ob>(
    cx: &'ob crate::core::gc::Context,
) -> crate::core::object::Object<'ob> {
    use crate::core::{cons::Cons, object::NIL};
    let mut list = NIL;
    // Preserve native order, including first-match semantics for duplicate names.
    let entries = std::env::vars_os().collect::<Vec<_>>();
    for (mut key, value) in entries.into_iter().rev() {
        key.push("=");
        key.push(value);
        let entry = match key.to_str() {
            Some(value) => cx.add(value.to_owned()),
            None => cx.add(key.as_encoded_bytes().to_vec()),
        };
        list = Cons::new(entry, list, cx).into();
    }
    list
}

defvar!(PROCESS_ENVIRONMENT, crate::emacs::initial_process_environment(cx));

use crate::core::{
    env::{Env, sym},
    gc::{Context, Rt},
    object::{NIL, Object, ObjectType, TRUE},
};
use anyhow::{Result, ensure};

fn environment_bytes(value: Object<'_>) -> Option<&[u8]> {
    match value.untag() {
        ObjectType::String(value) => Some(value.as_bytes()),
        ObjectType::ByteString(value) => Some(value),
        _ => None,
    }
}

#[defun]
fn getenv_internal<'ob>(
    variable: Object,
    environment: Option<Object>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let name = environment_bytes(variable)
        .ok_or_else(|| anyhow::anyhow!("Environment variable name must be a string"))?;
    ensure!(name.len() <= 65536, "Environment variable name exceeds limit");
    let explicit = environment.filter(|value| !value.is_nil());
    let entries = match explicit {
        Some(value) => value,
        None => env
            .variable_value(sym::PROCESS_ENVIRONMENT, cx)?
            .ok_or_else(|| anyhow::anyhow!("process-environment is unbound"))?,
    };
    for (index, entry) in entries.as_list()?.enumerate() {
        ensure!(index < 65536, "Environment list exceeds limit or is cyclic");
        let entry = entry?;
        let Some(bytes) = environment_bytes(entry) else {
            continue;
        };
        ensure!(bytes.len() <= 4 * 1024 * 1024, "Environment entry exceeds limit");
        if bytes == name {
            return Ok(if explicit.is_some() { TRUE } else { NIL });
        }
        if bytes.starts_with(name) && bytes.get(name.len()) == Some(&b'=') {
            let value = &bytes[name.len() + 1..];
            return Ok(match entry.untag() {
                ObjectType::String(_) => cx.add(std::str::from_utf8(value)?.to_owned()),
                _ => cx.add(value.to_vec()),
            });
        }
    }
    Ok(NIL)
}

/// Complete child environment. Callers must clear inherited environment first.
/// First entry wins; a bare name removes the variable, NAME= means empty.
pub(crate) fn process_environment_snapshot(
    env: &Rt<Env>,
    cx: &Context,
) -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    let entries = env
        .variable_value(sym::PROCESS_ENVIRONMENT, cx)?
        .ok_or_else(|| anyhow::anyhow!("process-environment is unbound"))?;
    let mut seen = std::collections::HashSet::new();
    let mut output = Vec::new();
    let mut total = 0usize;
    for (index, entry) in entries.as_list()?.enumerate() {
        ensure!(index < 65536, "Environment list exceeds limit or is cyclic");
        let entry = entry?;
        let bytes = environment_bytes(entry)
            .ok_or_else(|| anyhow::anyhow!("Child environment entries must be strings"))?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow::anyhow!("Environment size overflow"))?;
        ensure!(
            total <= 8 * 1024 * 1024 && !bytes.contains(&0),
            "Invalid or oversized child environment"
        );
        let split = bytes.iter().position(|byte| *byte == b'=');
        let name = split.map_or(bytes, |at| &bytes[..at]);
        ensure!(!name.is_empty(), "Child environment name must not be empty");
        if !seen.insert(name.to_vec()) {
            continue;
        }
        if let Some(at) = split {
            output.push((environment_os_string(name)?, environment_os_string(&bytes[at + 1..])?));
        }
    }
    Ok(output)
}

fn environment_os_string(bytes: &[u8]) -> Result<std::ffi::OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Ok(std::ffi::OsString::from(std::str::from_utf8(bytes)?))
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    use crate::core::{cons::Cons, gc::RootSet};
    use rune_core::macros::{list, rebind, root};
    #[test]
    fn actual_lisp_lookup_dynamic_nil_and_explicit_negative_match_gnu() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let source = r#"(progn (setq process-environment '("RUNE_A=first" "RUNE_A=second" "RUNE_B" "RUNE_B=later" "RUNE_EMPTY=")) (list (getenv-internal "RUNE_A") (let ((process-environment nil)) (getenv-internal "RUNE_A")) (getenv-internal "RUNE_B") (getenv-internal "RUNE_B" '("RUNE_B")) (getenv-internal "RUNE_EMPTY") (getenv-internal "RUNE_A" nil) (getenv-internal "A=B" '("A=B=x")) (getenv-internal "RUNE_A" '(42 "RUNE_A=ok"))))"#;
        let form = crate::reader::read(source, cx)?.0;
        root!(form, cx);
        let value = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
        assert_eq!(value.to_string(), "(\"first\" nil nil t \"\" \"first\" \"x\" \"ok\")");
        Ok(())
    }
    #[test]
    fn snapshot_is_complete_first_wins_owned_and_validated() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        env.set_var(
            sym::PROCESS_ENVIRONMENT,
            list!["RUNE_A=first", "RUNE_A=second", "RUNE_B", "RUNE_B=later", "RUNE_C="; cx],
        )?;
        let values = process_environment_snapshot(env, cx)?;
        assert_eq!(values, vec![("RUNE_A".into(), "first".into()), ("RUNE_C".into(), "".into())]);
        env.set_var(sym::PROCESS_ENVIRONMENT, NIL)?;
        cx.garbage_collect(true);
        assert_eq!(values[0].1, std::ffi::OsString::from("first"));
        assert!(process_environment_snapshot(env, cx)?.is_empty());
        for bad in [list!["RUNE_BAD=x\0y"; cx], list![42i64; cx], list!["=bad"; cx]] {
            env.set_var(sym::PROCESS_ENVIRONMENT, bad)?;
            assert!(process_environment_snapshot(env, cx).is_err());
        }
        let circular = Cons::new("RUNE_A=x", NIL, cx);
        circular.set_cdr(circular.into())?;
        env.set_var(sym::PROCESS_ENVIRONMENT, circular.into())?;
        assert!(process_environment_snapshot(env, cx).is_err());
        Ok(())
    }
    #[test]
    fn byte_names_and_values_are_never_unicode_coerced() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        env.set_var(sym::PROCESS_ENVIRONMENT, list![cx.add(b"RUNE_RAW=\xff\xfe".to_vec()); cx])?;
        let value = getenv_internal(cx.add(b"RUNE_RAW".to_vec()), None, env, cx)?;
        let ObjectType::ByteString(value) = value.untag() else {
            panic!("raw environment value coerced")
        };
        assert_eq!(&**value, &[0xff, 0xfe]);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let snapshot = process_environment_snapshot(env, cx)?;
            assert_eq!(snapshot[0].1.as_bytes(), &[0xff, 0xfe]);
        }
        Ok(())
    }
}
