//! Buffer editing utilities.
use crate::core::{
    env::{ArgSlice, Env},
    gc::{Context, Rt},
    object::{Object, ObjectType},
};
use anyhow::{Result, bail, ensure};
use rune_macros::defun;
use std::{fmt::Write as _, io::Write};

#[defun]
fn message(format_string: &str, args: &[Object]) -> Result<String> {
    let message = format(format_string, args)?;
    println!("MESSAGE: {message}");
    std::io::stdout().flush()?;
    Ok(message)
}

defvar!(MESSAGE_NAME);
defvar!(MESSAGE_TYPE, "new message");

#[defun]
fn format(string: &str, objects: &[Object]) -> Result<String> {
    let mut result = String::new();
    let mut arguments = objects.iter();
    let mut remaining = string;

    let mut escaped = false;
    let mut is_format_char = |c: char| {
        if escaped {
            escaped = false;
            false
        } else if c == '\\' {
            escaped = true;
            false
        } else {
            c == '%'
        }
    };
    while let Some(start) = remaining.find(&mut is_format_char) {
        result += &remaining[..start];
        let Some(specifier) = remaining.as_bytes().get(start + 1) else {
            bail!("Format string ends in middle of format specifier")
        };
        // "%%" inserts a single "%" in the output
        if *specifier == b'%' {
            result.push('%');
        } else {
            // TODO: currently handles all format types the same. Need to check the modifier characters.
            let Some(val) = arguments.next() else {
                bail!("Not enough arguments for format string")
            };
            match val.untag() {
                ObjectType::String(string) => write!(result, "{string}")?,
                obj => write!(result, "{obj}")?,
            }
        }
        remaining = &remaining[start + 2..];
    }
    result += remaining;
    ensure!(arguments.next().is_none(), "Too many arguments for format string");
    Ok(result)
}

#[defun]
fn format_message(string: &str, objects: &[Object]) -> Result<String> {
    let formatted = format(string, objects)?;
    // TODO: implement support for `text-quoting-style`.
    Ok(formatted
        .chars()
        .map(|c| if matches!(c, '`' | '\'') { '"' } else { c })
        .collect())
}

#[defun]
fn string_to_char(string: &str) -> char {
    string.chars().next().unwrap_or('\0')
}

#[defun]
fn char_to_string(chr: char) -> String {
    format!("{chr}")
}

#[defun]
pub(crate) fn insert(args: ArgSlice, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    let args = Rt::bind_slice(env.stack.arg_slice(args), cx);
    for arg in args {
        let mut encoded = [0u8; 4];
        let text = match arg.untag() {
            ObjectType::String(text) => &**text,
            ObjectType::Int(value) => {
                let code = u32::try_from(value)?;
                let Some(ch) = char::from_u32(code) else {
                    bail!("Invalid Unicode character {value}")
                };
                ch.encode_utf8(&mut encoded)
            }
            other => {
                bail!(crate::core::error::TypeError::new(crate::core::error::Type::String, other))
            }
        };
        let read = env.buffer_read(cx)?;
        env.editor_gateway()
            .splice(read.handle, read.epoch, read.point..read.point, text, cx)?;
    }
    Ok(())
}
#[defun]
pub(crate) fn goto_char(position: i64, env: &Rt<Env>, cx: &Context) -> Result<i64> {
    let read = env.buffer_read(cx)?;
    let index = usize::try_from(position.max(1))
        .unwrap_or(usize::MAX)
        .saturating_sub(1)
        .min(read.document.text().len_chars());
    env.editor_gateway().set_point(read.handle, index, cx)?;
    Ok(position)
}
#[defun]
pub(crate) fn point_max(env: &Rt<Env>, cx: &Context) -> Result<usize> {
    Ok(env.buffer_read(cx)?.document.text().len_chars() + 1)
}
#[defun]
pub(crate) fn point_min() -> usize {
    1
}
#[defun]
pub(crate) fn point_marker(env: &Rt<Env>, cx: &Context) -> Result<usize> {
    point(env, cx)
}
#[defun]
fn delete_region(start: usize, end: usize, env: &Rt<Env>, cx: &Context) -> Result<()> {
    let read = env.buffer_read(cx)?;
    let (start, end) = (start.min(end), start.max(end));
    ensure!(
        start > 0 && end <= read.document.text().len_chars() + 1,
        "Buffer range out of bounds"
    );
    env.editor_gateway()
        .splice(read.handle, read.epoch, start - 1..end - 1, "", cx)?;
    Ok(())
}
#[defun]
fn bolp(env: &Rt<Env>, cx: &Context) -> Result<bool> {
    let read = env.buffer_read(cx)?;
    Ok(read.point == 0
        || read.document.text().char_at(rune_text::CharPos(read.point - 1)) == Some('\n'))
}
#[defun]
fn point(env: &Rt<Env>, cx: &Context) -> Result<usize> {
    Ok(env.buffer_read(cx)?.point + 1)
}

#[defun]
fn system_name() -> String {
    hostname::get()
        .expect("Failed to get hostname")
        .into_string()
        .expect("Failed to convert OsString to String")
}

#[defun]
fn user_uid() -> usize {
    ids::effective_uid()
}

#[defun]
fn user_real_uid() -> usize {
    ids::real_uid()
}

#[defun]
fn group_gid() -> usize {
    ids::effective_gid()
}

#[defun]
fn group_real_gid() -> usize {
    ids::real_gid()
}

#[cfg(unix)]
mod ids {
    pub(super) fn effective_uid() -> usize {
        unsafe { libc::geteuid() as usize }
    }

    pub(super) fn real_uid() -> usize {
        unsafe { libc::getuid() as usize }
    }

    pub(super) fn effective_gid() -> usize {
        unsafe { libc::getegid() as usize }
    }

    pub(super) fn real_gid() -> usize {
        unsafe { libc::getgid() as usize }
    }
}

#[cfg(not(unix))]
mod ids {
    /// Windows accounts have no POSIX ids. Emacs falls back to this value when it
    /// can't derive one from the account SID, so report the same thing.
    const UNKNOWN_ID: usize = 123;

    pub(super) fn effective_uid() -> usize {
        UNKNOWN_ID
    }

    pub(super) fn real_uid() -> usize {
        UNKNOWN_ID
    }

    pub(super) fn effective_gid() -> usize {
        UNKNOWN_ID
    }

    pub(super) fn real_gid() -> usize {
        UNKNOWN_ID
    }
}

#[defun]
fn emacs_pid() -> usize {
    std::process::id() as usize
}

#[cfg(test)]
mod test {
    use crate::core::object::NIL;
    use crate::{
        buffer::{get_buffer_create, set_buffer},
        core::gc::RootSet,
    };
    use rune_core::macros::root;

    use super::*;

    #[test]
    fn test_format() {
        assert_eq!(&format("%s", &[1.into()]).unwrap(), "1");
        assert_eq!(&format("foo-%s", &[2.into()]).unwrap(), "foo-2");
        assert_eq!(&format("%%", &[]).unwrap(), "%");
        assert_eq!(&format("_%%_", &[]).unwrap(), "_%_");
        assert_eq!(&format("foo-%s %s", &[3.into(), 4.into()]).unwrap(), "foo-3 4");
        let sym = crate::core::env::sym::FUNCTION.into();
        assert_eq!(&format("%s", &[sym]).unwrap(), "function");

        assert!(&format("%s", &[]).is_err());
        assert!(&format("%s", &[1.into(), 2.into()]).is_err());

        assert!(format("`%s' %s%s%s", &[0.into(), 1.into(), 2.into(), 3.into()]).is_ok());
    }

    #[test]
    fn test_insert() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        let buffer = get_buffer_create(cx.add("test_insert"), Some(NIL), cx).unwrap();
        set_buffer(buffer, env, cx).unwrap();
        cx.garbage_collect(true);
        env.stack.push(104);
        env.stack.push(101);
        env.stack.push(108);
        env.stack.push(108);
        env.stack.push(111);
        insert(ArgSlice::new(5), env, cx).unwrap();
        assert_eq!(env.buffer_read(cx).unwrap().document.text().to_string(), "hello");
    }

    #[test]
    fn test_delete_region() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        let buffer = get_buffer_create(cx.add("test_delete_region"), Some(NIL), cx).unwrap();
        set_buffer(buffer, env, cx).unwrap();
        cx.garbage_collect(true);
        env.stack.push(cx.add("hello"));
        env.stack.push(cx.add(" world"));
        insert(ArgSlice::new(2), env, cx).unwrap();

        assert_eq!(env.buffer_read(cx).unwrap().document.text().to_string(), "hello world");
        delete_region(2, 4, env, cx).unwrap();
        assert_eq!(env.buffer_read(cx).unwrap().document.text().to_string(), "hlo world");
    }
}

#[cfg(test)]
mod recovered_point_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn signed_one_based_points_are_character_offsets() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        assert_eq!(point(env, cx)?, 1);
        let read = env.buffer_read(cx)?;
        env.editor_gateway().splice(read.handle, read.epoch, 0..0, "aé🙂z", cx)?;
        for (requested, expected) in
            [(-10, 1), (0, 1), (1, 1), (2, 2), (3, 3), (5, 5), (99, 5), (i64::MAX, 5)]
        {
            assert_eq!(goto_char(requested, env, cx)?, requested);
            assert_eq!(point(env, cx)?, expected);
            assert_eq!(point_marker(env, cx)?, expected)
        }
        goto_char(1, env, cx)?;
        assert!(bolp(env, cx)?);
        goto_char(2, env, cx)?;
        assert!(!bolp(env, cx)?);
        Ok(())
    }
}
