//! Buffer editing utilities.
use crate::core::{
    env::{ArgSlice, Env},
    gc::{Context, Rt},
    object::{Object, ObjectType},
};
use anyhow::{Result, bail, ensure};
use rune_macros::defun;
use std::io::Write;

fn plain_format_control<'ob>(object: Object<'ob>) -> Result<&'ob str> {
    match object.untag() {
        ObjectType::String(text) => {
            ensure!(!text.has_properties(), "Property-bearing format controls are unsupported");
            Ok(text)
        }
        ObjectType::ByteString(bytes) => {
            ensure!(!bytes.has_properties(), "Property-bearing format controls are unsupported");
            ensure!(bytes.is_ascii(), "Non-ASCII unibyte format controls are unsupported");
            Ok(std::str::from_utf8(bytes)?)
        }
        _ => bail!("Format control must be a string"),
    }
}

#[defun]
pub(crate) fn message<'ob>(format_string: Object, args: &[Object], cx: &'ob Context) -> Result<Object<'ob>> {
    if format_string.is_nil() {
        cx.replace_echo_message(None)?;
        println!("MESSAGE: ");
        std::io::stdout().flush()?;
        return Ok(crate::core::object::NIL);
    }
    let message = format(plain_format_control(format_string)?, args)?;
    ensure!(message.len() <= crate::echo::MAX_ECHO_BYTES, "Echo request exceeds 16 MiB retention limit");
    let mut retained = String::new();
    retained.try_reserve_exact(message.len())?;
    retained.push_str(&message);
    cx.replace_echo_message(Some(retained))?;
    println!("MESSAGE: {message}");
    std::io::stdout().flush()?;
    Ok(cx.add(message))
}

defvar!(MESSAGE_NAME);
defvar!(MESSAGE_TYPE, "new message");

#[path = "format.rs"]
mod format_impl;

#[defun(name = "format")]
fn format_lisp(string: Object, objects: &[Object]) -> Result<String> {
    format(plain_format_control(string)?, objects)
}
fn format(string: &str, objects: &[Object]) -> Result<String> {
    format_impl::format(string, objects)
}

#[defun(name = "format-message")]
fn format_message_lisp(string: Object, objects: &[Object]) -> Result<String> {
    format_message(plain_format_control(string)?, objects)
}
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
            ObjectType::String(text) => {
                &**text
            },
            ObjectType::ByteString(bytes) => {
                ensure!(bytes.is_ascii(), "Non-ASCII unibyte insertion into Unicode buffers is unsupported");
                // ASCII has identical byte/scalar meaning. Do not decode arbitrary
                // raw bytes as UTF-8 or silently turn them into Latin-1 characters.
                std::str::from_utf8(bytes)?
            }
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
        let gateway = env.editor_gateway();
        let runs = match arg.untag() {
            ObjectType::String(_) | ObjectType::ByteString(_) =>
                crate::core::object::string_property_runs(*arg)?,
            _ => Vec::new(),
        };
        if runs.is_empty() {
            gateway.splice(read.handle, read.epoch, read.point..read.point, text, cx)?;
        } else {
            let mut spans = Vec::new();
            spans.try_reserve_exact(runs.len())?;
            for run in runs {
                let mut cells = Vec::new();
                for cell in run.plist.as_list()? {
                    ensure!(cells.len() < 8192, "Inserted string property plist exceeds limit");
                    cells.push(cell?);
                }
                ensure!(cells.len() % 2 == 0, "Odd inserted string property plist");
                let copied = crate::fns::slice_into_list(&cells, None, cx);
                // SAFETY: the source argument is rooted on this Env's stack;
                // its reachable values belong to cx/audited globals. New plist
                // spines use cx, retain shallow values, and no callback or GC
                // occurs while shared cx stages the authenticated handles.
                let handle = unsafe { gateway.intern_property(copied, cx)? };
                spans.push((rune_text::CharPos(run.start)..rune_text::CharPos(run.end), handle));
            }
            gateway.insert_with_properties(read.handle, read.epoch, read.point, text, spans, cx)?;
        }
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
/// Character count, independent of point and UTF-8 byte length.
#[defun]
fn buffer_size(
    buffer: Option<crate::core::object::Gc<&crate::core::object::LispBuffer>>,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<usize> {
    let read = match buffer {
        Some(buffer) => buffer.untag().read(cx)?,
        None => env.buffer_read(cx)?,
    };
    Ok(read.document.text().len_chars())
}

/// Full current buffer text. Lisp strings do not yet represent text properties;
/// reject that unsupported case rather than silently returning a lossy string.
#[defun]
fn buffer_string(env: &Rt<Env>, cx: &Context) -> Result<String> {
    let read = env.buffer_read(cx)?;
    for (_, id) in read.document.properties().runs() {
        ensure!(read.document.resolve(id.0, cx)?.is_nil(),
            "buffer-string of propertied text is not implemented");
    }
    Ok(read.document.text().to_string())
}

#[defun]
fn buffer_substring_no_properties(start: i64, end: i64, env: &Rt<Env>, cx: &Context) -> Result<String> {
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    let (start, end) = (start.min(end), start.max(end));
    ensure!(start >= 1, "Buffer range out of bounds");
    let start = usize::try_from(start - 1)?;
    let end = usize::try_from(end - 1)?;
    ensure!(end <= text.len_chars(), "Buffer range out of bounds");
    let bytes = text.char_to_byte(rune_text::CharPos(start))?
        ..text.char_to_byte(rune_text::CharPos(end))?;
    let mut result = String::new();
    for chunk in text.chunks(bytes)? {
        result.push_str(chunk);
    }
    Ok(result)
}

#[defun]
fn erase_buffer(env: &Rt<Env>, cx: &Context) -> Result<()> {
    let read = env.buffer_read(cx)?;
    // GNU leaves an already-empty buffer unmodified, with no undo entry.
    if read.document.text().is_empty() {
        return Ok(());
    }
    env.editor_gateway().splice(read.handle, read.epoch, 0..read.document.text().len_chars(), "", cx)?;
    Ok(())
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
fn character_at_point(position: Option<i64>, before: bool, env: &Rt<Env>, cx: &Context) -> Result<Option<usize>> {
    let read = env.buffer_read(cx)?;
    let index = match position {
        Some(position) => usize::try_from(position).ok().and_then(|p| p.checked_sub(if before { 2 } else { 1 })),
        None => if before { read.point.checked_sub(1) } else { Some(read.point) },
    };
    Ok(index.and_then(|i| read.document.text().char_at(rune_text::CharPos(i))).map(|c| c as usize))
}
#[defun]
fn char_after(position: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<Option<usize>> {
    character_at_point(position, false, env, cx)
}
#[defun]
fn char_before(position: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<Option<usize>> {
    character_at_point(position, true, env, cx)
}
fn line_position(count: Option<i64>, end: bool, env: &Rt<Env>, cx: &Context) -> Result<usize> {
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    let current = text.char_to_line(rune_text::CharPos(read.point))?.0;
    let target = current as i128 + i128::from(count.unwrap_or(1)) - 1;
    if target < 0 { return Ok(1); }
    let last = text.summary().newlines;
    if target > last as i128 { return Ok(text.len_chars() + 1); }
    let target = target as usize;
    if end {
        Ok(if target == last { text.len_chars() + 1 } else { text.line_to_char(rune_text::LinePos(target + 1))?.0 })
    } else {
        Ok(text.line_to_char(rune_text::LinePos(target))?.0 + 1)
    }
}
#[defun]
fn line_beginning_position(count: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<usize> {
    line_position(count, false, env, cx)
}
#[defun]
fn line_end_position(count: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<usize> {
    line_position(count, true, env, cx)
}

#[defun]
fn line_number_at_pos(position: Option<i64>, _absolute: crate::core::object::OptionalFlag, env: &Rt<Env>, cx: &Context) -> Result<usize> {
    let read = env.buffer_read(cx)?;
    let index = match position {
        Some(position) => { ensure!(position >= 1, "Buffer position out of bounds"); usize::try_from(position - 1)? },
        None => read.point,
    };
    ensure!(index <= read.document.text().len_chars(), "Buffer position out of bounds");
    Ok(read.document.text().char_to_line(rune_text::CharPos(index))?.0 + 1)
}
#[defun]
fn count_lines(start: i64, end: i64, env: &Rt<Env>, cx: &Context) -> Result<usize> {
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    let (start, end) = (start.min(end), start.max(end));
    ensure!(start >= 1, "Buffer range out of bounds");
    let start = usize::try_from(start - 1)?;
    let end = usize::try_from(end - 1)?;
    ensure!(end <= text.len_chars(), "Buffer range out of bounds");
    if start == end { return Ok(0); }
    let first = text.char_to_line(rune_text::CharPos(start))?.0;
    let last = text.char_to_line(rune_text::CharPos(end))?.0;
    let final_partial = end != text.line_to_char(rune_text::LinePos(last))?.0;
    Ok(last - first + usize::from(final_partial))
}
#[defun]
fn forward_line(count: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<i64> {
    let count = count.unwrap_or(1);
    let read = env.buffer_read(cx)?;
    let text = read.document.text();
    let current = text.char_to_line(rune_text::CharPos(read.point))?.0;
    let last = text.summary().newlines;
    let target = current as i128 + i128::from(count);
    let point = if target < 0 { 0 } else if target > last as i128 { text.len_chars() }
        else { text.line_to_char(rune_text::LinePos(target as usize))?.0 };
    let remaining = if count < 0 {
        (i128::from(count) + current as i128).min(0)
    } else {
        // A nonempty terminal partial line counts as one forward line only
        // when point has not already reached end of buffer.
        let partial = read.point < text.len_chars()
            && text.char_at(rune_text::CharPos(text.len_chars() - 1)) != Some('\n');
        (i128::from(count) - (last - current) as i128 - i128::from(partial)).max(0)
    };
    env.editor_gateway().set_point(read.handle, point, cx)?;
    Ok(i64::try_from(remaining)?)
}
#[defun]
fn beginning_of_line(count: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<()> {
    let position = line_position(count, false, env, cx)?;
    env.editor_gateway().set_point(env.current_buffer_handle(), position - 1, cx)
}
#[defun]
fn end_of_line(count: Option<i64>, env: &Rt<Env>, cx: &Context) -> Result<()> {
    let position = line_position(count, true, env, cx)?;
    env.editor_gateway().set_point(env.current_buffer_handle(), position - 1, cx)
}

/// Unicode buffer byte coordinates are one-based UTF-8 offsets.
#[defun]
fn position_bytes(position: i64, env: &Rt<Env>, cx: &Context) -> Result<Option<usize>> {
    let read = env.buffer_read(cx)?;
    let Some(index) = usize::try_from(position).ok().and_then(|p| p.checked_sub(1)) else { return Ok(None) };
    if index > read.document.text().len_chars() { return Ok(None); }
    Ok(Some(read.document.text().char_to_byte(rune_text::CharPos(index))?.0 + 1))
}
#[defun]
fn byte_to_position(byte: i64, env: &Rt<Env>, cx: &Context) -> Result<Option<usize>> {
    let read = env.buffer_read(cx)?;
    let Some(mut index) = usize::try_from(byte).ok().and_then(|p| p.checked_sub(1)) else { return Ok(None) };
    let text = read.document.text();
    if index > text.len_bytes() { return Ok(None); }
    // GNU rounds an interior byte back to the containing character. In UTF-8
    // at most three continuation bytes precede the next scalar boundary.
    for _ in 0..4 {
        match text.byte_to_char(rune_text::BytePos(index)) {
            Ok(position) => return Ok(Some(position.0 + 1)),
            Err(rune_text::TextError::NotCharBoundary) => index -= 1,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("Invalid UTF-8 character boundary")
}
#[defun]
fn following_char(env: &Rt<Env>, cx: &Context) -> Result<usize> {
    Ok(character_at_point(None, false, env, cx)?.unwrap_or(0))
}
#[defun]
fn preceding_char(env: &Rt<Env>, cx: &Context) -> Result<usize> {
    Ok(character_at_point(None, true, env, cx)?.unwrap_or(0))
}
#[defun]
fn bobp(env: &Rt<Env>, cx: &Context) -> Result<bool> {
    Ok(env.buffer_read(cx)?.point == 0)
}
#[defun]
fn eobp(env: &Rt<Env>, cx: &Context) -> Result<bool> {
    let read = env.buffer_read(cx)?;
    Ok(read.point == read.document.text().len_chars())
}
#[defun]
fn eolp(env: &Rt<Env>, cx: &Context) -> Result<bool> {
    let read = env.buffer_read(cx)?;
    Ok(matches!(read.document.text().char_at(rune_text::CharPos(read.point)), None | Some('\n')))
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
        assert_eq!(&format("%s", &[1.into(), 2.into()]).unwrap(), "1");

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
    fn ascii_unibyte_insert_matches_gnu() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "λ" (make-string 2 10) "🙂")
          (list (equal (buffer-substring-no-properties 1 (point-max)) "λ\n\n🙂") (point)))"#);
    }

    #[test]
    fn raw_unibyte_insert_rejects_without_editing_that_argument() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        env.stack.push(cx.add(vec![b'a', 0xff]));
        let before = env.buffer_read(cx).unwrap();
        assert!(insert(ArgSlice::new(1), env, cx).is_err());
        let after = env.buffer_read(cx).unwrap();
        assert_eq!(after.epoch, before.epoch);
        assert_eq!(after.point, before.point);
        assert_eq!(after.document.text().to_string(), "");
    }

    #[test]
    fn annotated_unicode_insert_matches_gnu_with_plain_gaps() {
        crate::assert_elprop!(r#"(let* ((value (list 42))
          (s (concat "a" (propertize "λ🙂" 'face value) "z")))
          (erase-buffer) (insert "pre") (goto-char 2) (insert s)
          (list (equal (buffer-substring-no-properties 1 (point-max)) "paλ🙂zre")
                (point) (get-text-property 2 'face)
                (eq (get-text-property 3 'face) value)
                (eq (get-text-property 4 'face) value)
                (get-text-property 5 'face)))"#);
    }
    #[test]
    fn annotated_ascii_unibyte_insert_matches_gnu() {
        crate::assert_elprop!(r#"(let ((value (list 42)))
          (erase-buffer) (insert (propertize (make-string 2 10) 'face value))
          (list (equal (buffer-substring-no-properties 1 (point-max)) "\n\n")
                (point) (eq (get-text-property 1 'face) value)
                (eq (get-text-property 2 'face) value)))"#);
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

#[cfg(test)]
mod owner_buffer_primitives_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn empty_erase_preserves_revision_modified_flag_and_history() -> Result<()> {
        use rune_editor::{Command, EditorHost};
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new(cx)?), cx);
        let g = env.editor_gateway();
        let before = env.buffer_read(cx)?;
        let mut native = g.host(cx)?;
        assert!(!native.snapshot()?.buffers[0].modified);
        erase_buffer(env, cx)?;
        assert_eq!(env.buffer_read(cx)?.epoch, before.epoch);
        assert!(!native.snapshot()?.buffers[0].modified);
        assert!(matches!(native.execute(Command::Undo), Err(rune_editor::EditorError::NoUndo)));
        Ok(())
    }
    #[test]
    fn lisp_empty_and_unicode_boundary_queries_match_gnu_without_moving_point() {
        crate::assert_elprop!(r#"(progn (erase-buffer)
          (let ((empty (list (bobp) (eobp) (bolp) (eolp) (following-char) (preceding-char))))
            (insert "é🙂\n") (goto-char 1)
            (list empty
                  (list (bobp) (eobp) (following-char) (preceding-char) (point))
                  (progn (goto-char 2) (list (following-char) (preceding-char) (point)))
                  (progn (goto-char 4) (list (bobp) (eobp) (bolp) (eolp) (following-char) (preceding-char) (point))))))"#);
    }
    #[test]
    fn indexed_line_counts_match_gnu_reversed_partial_and_invalid_ranges() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "aé\n🙂x\nef") (goto-char 5)
          (list (line-number-at-pos) (line-number-at-pos nil t)
            (mapcar (function (lambda (p) (condition-case nil (line-number-at-pos p) (error 'bad))))
                    (quote (-1 0 1 3 4 6 7 9 10 99)))
            (mapcar (function (lambda (r) (condition-case nil (apply (function count-lines) r) (error 'bad))))
                    (quote ((1 1) (1 2) (2 3) (2 4) (3 4) (4 7) (2 8) (8 2) (0 3) (1 99))))
            (point)))"#);
        crate::assert_elprop!(r#"(progn (erase-buffer)
            (list (line-number-at-pos) (count-lines 1 1)))"#);
    }
    #[test]
    fn forward_line_matches_gnu_partial_lines_remainders_and_eof() {
        for fixture in ["ab\ncd\nef", "ab\n", "ab", ""] {
            let literal = fixture.replace('\n', "\\n");
            for at_end in [false, true] {
                crate::assert_elprop!(r#"(progn (erase-buffer) (insert "{}") (goto-char {})
                  (mapcar (function (lambda (n) (save-excursion (list n (forward-line n) (point)))))
                    (quote (-99 -2 -1 0 1 2 3 99))))"#, literal, if at_end { 999 } else { 2 });
            }
        }
    }
    #[test]
    fn line_movement_matches_gnu_unicode_offsets_and_clamping() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "aé\n🙂x\nef") (goto-char 5)
          (list (beginning-of-line) (point) (end-of-line) (point)
                (beginning-of-line 0) (point) (end-of-line 2) (point)
                (end-of-line -99) (point) (beginning-of-line 99) (point)
                (equal (buffer-substring-no-properties 1 (point-max)) "aé\n🙂x\nef")))"#);
        crate::assert_elprop!(r#"(progn (erase-buffer)
          (list (beginning-of-line) (point) (end-of-line 99) (point)))"#);
    }
    #[test]
    fn indexed_line_boundaries_match_gnu_unicode_empty_and_offsets() {
        crate::assert_elprop!(r#"(progn (erase-buffer)
          (let ((empty (list (line-beginning-position) (line-end-position))))
            (insert "aé\n🙂x\nef") (goto-char 5)
            (list empty
                  (mapcar (function line-beginning-position) (quote (-99 0 1 2 99)))
                  (mapcar (function line-end-position) (quote (-99 0 1 2 99)))
                  (point))))"#);
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "é\n")
          (list (line-beginning-position) (line-end-position)
                (line-beginning-position 0) (line-end-position 0) (point)))"#);
    }
    #[test]
    fn unicode_byte_coordinates_match_gnu_interior_rounding() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "aé🙂z") (goto-char 3)
          (list (mapcar (function position-bytes) (quote (0 1 2 3 4 5 6)))
                (mapcar (function byte-to-position) (quote (0 1 2 3 4 5 6 7 8 9 10)))
                (point)))"#);
    }
    #[test]
    fn lisp_character_queries_match_gnu_unicode_and_boundaries() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "aé🙂\nz") (goto-char 3)
          (list (char-after) (char-before) (char-after 0) (char-before 0)
                (char-after 99) (char-before 99) (char-before 1) (char-after 6)
                (eolp) (progn (goto-char 4) (eolp))
                (progn (goto-char 6) (list (char-after) (char-before) (eolp)))))"#);
    }
    #[test]
    fn persistent_owner_lisp_copy_erase_and_native_history_share_storage() -> Result<()> {
        use rune_editor::{Command, Document, EditorHost};
        crate::editor_document::with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source("(insert \"aé🙂z\") (setq owner-copy (buffer-substring-no-properties 2 4)) (if (= (buffer-size) 4) nil (error \"wrong scalar size\"))")
                .map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "aé🙂z");
            host.evaluate_source("(erase-buffer)").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "");
            host.execute(Command::Undo)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "aé🙂z");
            host.execute(Command::Redo)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "");
            host.evaluate_source("(insert owner-copy) (garbage-collect)").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.buffers[0].document.to_text(), "é🙂");
            Ok(())
        })
    }
    #[test]
    fn lisp_buffer_primitives_match_gnu_unicode_and_noncurrent_size() {
        crate::assert_elprop!(r#"(save-current-buffer
          (let ((b (get-buffer-create "owner-primitive-oracle")))
            (set-buffer b) (erase-buffer) (insert "aé🙂z")
            (list (buffer-size b) (buffer-substring-no-properties 4 2)
                  (buffer-substring-no-properties 5 5)
                  (progn (erase-buffer) (list (buffer-size) (point))))))"#);
        crate::assert_elprop!(r#"(save-current-buffer
          (let ((a (get-buffer-create "owner-size-a"))
                (b (get-buffer-create "owner-size-b")))
            (set-buffer a) (erase-buffer) (insert "é🙂")
            (set-buffer b) (erase-buffer) (insert "x")
            (list (buffer-size a) (buffer-size b) (buffer-size))))"#);
    }
    #[test]
    fn lisp_substring_errors_do_not_mutate_text_or_point() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "aé🙂z") (goto-char 3)
          (list (condition-case nil (buffer-substring-no-properties -1 2) (error t))
                (condition-case nil (buffer-substring-no-properties 1 6) (error t))
                (buffer-size) (point) (buffer-substring-no-properties 1 5)))"#);
    }
    #[test]
    fn unicode_substrings_validate_before_edit_and_erase_uses_same_owner() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new(cx)?), cx);
        let g = env.editor_gateway();
        let read = env.buffer_read(cx)?;
        g.splice(read.handle, read.epoch, 0..0, "aé🙂z", cx)?;
        assert_eq!(buffer_size(None, env, cx)?, 4);
        assert_eq!(buffer_substring_no_properties(4, 2, env, cx)?, "é🙂");
        assert_eq!(buffer_substring_no_properties(5, 5, env, cx)?, "");
        assert!(buffer_substring_no_properties(0, 2, env, cx).is_err());
        assert!(buffer_substring_no_properties(1, 6, env, cx).is_err());
        assert_eq!(buffer_size(None, env, cx)?, 4);
        erase_buffer(env, cx)?;
        assert_eq!(buffer_size(None, env, cx)?, 0);
        assert_eq!(point(env, cx)?, 1);
        use rune_editor::EditorHost;
        let mut native = g.host(cx)?;
        native.execute(rune_editor::Command::Undo)?;
        assert_eq!(buffer_substring_no_properties(1, 5, env, cx)?, "aé🙂z");
        Ok(())
    }
}

#[cfg(test)]
mod buffer_string_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::{root, rebind};

    #[test]
    fn actual_lisp_buffer_string_returns_all_unicode_text_without_moving_point() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let source = "(progn (insert \"aé🙂\nsecond\") (goto-char 2) (list (buffer-string) (point)))";
        let form = crate::reader::read(source, cx)?.0;
        root!(form, cx);
        let result = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
        assert_eq!(result.to_string(), "(\"aé🙂\nsecond\" 2)");
        Ok(())
    }

    #[test]
    fn buffer_string_is_current_owner_specific_and_rejects_property_loss() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        assert_eq!(buffer_string(env, cx)?, "");
        let gateway = env.editor_gateway();
        let read = env.buffer_read(cx)?;
        gateway.splice(read.handle, read.epoch, 0..0, "abc", cx)?;
        let read = env.buffer_read(cx)?;
        let property = unsafe { gateway.intern_property(cx.add("retained-property"), cx) }?;
        let prepared = gateway.prepare_properties(&read, rune_text::CharPos(0)..rune_text::CharPos(1), &property, cx)?;
        gateway.publish_properties(prepared, cx)?;
        assert!(buffer_string(env, cx).is_err());
        let other = gateway.create(" *buffer-string-other*", "other", cx)?;
        env.set_current_buffer(other, cx)?;
        assert_eq!(buffer_string(env, cx)?, "other");
        Ok(())
    }
}
