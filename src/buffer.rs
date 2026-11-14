//! Buffer metadata and identities resolve through the Context-owned Session.
use crate::{
    core::{
        env::Env,
        error::{Type, TypeError},
        gc::{Context, Rt},
        object::{Gc, LispBuffer, NIL, Object, ObjectType, OptionalFlag},
    },
    fns::slice_into_list,
};
use anyhow::{Result, bail};
use rune_macros::defun;
#[defun]
fn current_buffer<'ob>(env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    Ok(cx.add(LispBuffer::for_handle(env.current_buffer_handle(), cx)?))
}
fn resolve_buffer<'ob>(value: Object, cx: &'ob Context) -> Result<&'ob LispBuffer> {
    match value.untag() {
        ObjectType::Buffer(buffer) => {
            buffer.read(cx)?;
            Ok(cx.bind(buffer))
        }
        ObjectType::String(name) => {
            let gateway = cx.editor_gateway()?;
            let Some(handle) = gateway.lookup(name, cx)? else { bail!("No buffer named {name}") };
            LispBuffer::for_handle(handle, cx)
        }
        other => Err(TypeError::new(Type::BufferOrName, other).into()),
    }
}
#[defun]
pub(crate) fn set_buffer<'ob>(
    value: Object,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let buffer = resolve_buffer(value, cx)?;
    env.set_buffer(buffer, cx)?;
    Ok(cx.add(buffer))
}
#[defun]
fn set_buffer_modified_p(flag: Object) -> Object {
    flag
}
#[defun]
fn buffer_live_p(value: Object, cx: &Context) -> bool {
    value.buffer().is_some_and(|buffer| buffer.read(cx).is_ok())
}
#[defun]
fn buffer_name(
    buffer: Option<Gc<&LispBuffer>>,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<Option<String>> {
    match buffer {
        Some(buffer) => {
            let gateway = cx.editor_gateway()?;
            if !gateway.list(cx)?.contains(&buffer.untag().handle()) {
                return Ok(None);
            }
            Ok(Some(buffer.untag().read(cx)?.name))
        }
        None => Ok(Some(env.buffer_read(cx)?.name)),
    }
}
#[defun]
fn rename_buffer(name: &str, unique: OptionalFlag, env: &Rt<Env>, cx: &Context) -> Result<String> {
    env.editor_gateway()
        .rename(env.current_buffer_handle(), name, unique.is_some(), cx)
}
#[defun]
pub(crate) fn get_buffer_create<'ob>(
    value: Object<'ob>,
    _inhibit: Option<Object>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    match value.untag() {
        ObjectType::String(name) => {
            let gateway = cx.editor_gateway()?;
            let handle = match gateway.lookup(name, cx)? {
                Some(handle) => handle,
                None => gateway.create(name, "", cx)?,
            };
            Ok(cx.add(LispBuffer::for_handle(handle, cx)?))
        }
        ObjectType::Buffer(_) => Ok(value),
        other => Err(TypeError::new(Type::BufferOrName, other).into()),
    }
}
#[defun]
pub(crate) fn get_buffer<'ob>(value: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    match value.untag() {
        ObjectType::String(name) => {
            let gateway = cx.editor_gateway()?;
            match gateway.lookup(name, cx)? {
                Some(handle) => Ok(cx.add(LispBuffer::for_handle(handle, cx)?)),
                None => Ok(NIL),
            }
        }
        ObjectType::Buffer(_) => Ok(value),
        other => Err(TypeError::new(Type::BufferOrName, other).into()),
    }
}
#[defun]
pub(crate) fn generate_new_buffer_name(
    name: &str,
    ignore: Option<&str>,
    cx: &Context,
) -> Result<String> {
    let gateway = cx.editor_gateway()?;
    let mut names = std::collections::HashSet::new();
    for handle in gateway.list(cx)? {
        names.insert(gateway.read(handle, cx)?.name);
    }
    let valid = |candidate: &str| ignore == Some(candidate) || !names.contains(candidate);
    if valid(name) {
        return Ok(name.to_owned());
    }
    if name.starts_with(' ') {
        loop {
            let candidate = format!("{name}-{}", rand::random::<u32>());
            if valid(&candidate) {
                return Ok(candidate);
            }
        }
    }
    for index in 2usize.. {
        let candidate = format!("{name}<{index}>");
        if valid(&candidate) {
            return Ok(candidate);
        }
    }
    bail!("Buffer name identifiers exhausted")
}
#[defun]
fn kill_buffer(value: Option<Object>, cx: &Context, env: &mut Rt<Env>) -> bool {
    let handle = match value {
        Some(value) => match resolve_buffer(value, cx) {
            Ok(buffer) => buffer.handle(),
            Err(_) => return false,
        },
        None => env.current_buffer_handle(),
    };
    let Ok(fallback) = env.editor_gateway().kill(handle, true, cx) else { return false };
    if env.current_buffer_handle() == handle {
        let _ = env.set_current_buffer(fallback, cx);
    }
    true
}
#[defun]
fn buffer_base_buffer(_buffer: OptionalFlag) -> bool {
    false
}
#[defun]
fn get_file_buffer(_filename: &str) -> bool {
    false
}
#[defun]
fn buffer_list<'ob>(_frame: OptionalFlag, cx: &'ob Context) -> Result<Object<'ob>> {
    let gateway = cx.editor_gateway()?;
    let mut buffers = Vec::new();
    for handle in gateway.list(cx)? {
        buffers.push(cx.add(LispBuffer::for_handle(handle, cx)?));
    }
    Ok(slice_into_list(&buffers, None, cx))
}

// TODO: buffer local
defvar!(FILL_COLUMN, 70);
defvar!(INDENT_TABS_MODE);
defvar!(LEFT_MARGIN, 0);
defvar!(INHIBIT_COMPACTING_FONT_CACHES);
defvar!(NO_UPDATE_AUTOLOADS);
defvar!(TAB_WIDTH, 8);
defvar!(TRUNCATE_LINES);
defvar!(WORD_WRAP);
defvar!(BIDI_DISPLAY_REORDERING);
defvar!(BUFFER_FILE_NAME);

#[cfg(test)]
mod test {
    use super::*;
    use crate::core::gc::RootSet;

    #[test]
    fn test_gen_new_buffer_name() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);

        let name = "gen_buffer_test";
        let new_name = generate_new_buffer_name(name, None, cx).unwrap();
        assert_eq!(new_name, "gen_buffer_test");

        get_buffer_create(cx.add(name), Some(NIL), cx).unwrap();
        let new_name = generate_new_buffer_name(name, None, cx).unwrap();
        assert_eq!(new_name, "gen_buffer_test<2>");

        get_buffer_create(cx.add("gen_buffer_test<2>"), Some(NIL), cx).unwrap();
        let new_name = generate_new_buffer_name(name, None, cx).unwrap();
        assert_eq!(new_name, "gen_buffer_test<3>");

        let new_name = generate_new_buffer_name(name, Some("gen_buffer_test<2>"), cx).unwrap();
        assert_eq!(new_name, "gen_buffer_test<2>");

        let new_name = generate_new_buffer_name(" gen_buffer_test", None, cx).unwrap();
        assert_eq!(new_name, " gen_buffer_test");

        get_buffer_create(cx.add(" gen_buffer_test"), Some(NIL), cx).unwrap();
        let new_name = generate_new_buffer_name(" gen_buffer_test", None, cx).unwrap();
        assert!(new_name.starts_with(" gen_buffer_test-"));
    }

    #[test]
    fn test_create_buffer() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let buffer = get_buffer_create(cx.add("test_create_buffer"), Some(NIL), cx).unwrap();
        assert!(matches!(buffer.untag(), ObjectType::Buffer(_)));
    }
}
