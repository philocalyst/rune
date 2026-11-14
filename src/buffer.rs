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
fn set_buffer_modified_p(flag: Object, env: &Rt<Env>, cx: &Context) -> Result<()> {
    env.editor_gateway().set_modified(env.current_buffer_handle(), !flag.is_nil(), cx)
}
#[defun]
fn buffer_modified_p(buffer: Option<Gc<&LispBuffer>>, env: &Rt<Env>, cx: &Context) -> Result<bool> {
    let handle = buffer.map_or_else(|| env.current_buffer_handle(), |buffer| buffer.untag().handle());
    env.editor_gateway().modified(handle, cx)
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

#[cfg(test)]
mod owner_modified_primitive_tests {
    #[test]
    fn modified_history_restores_clean_savepoints_bidirectionally() -> anyhow::Result<()> {
        use rune_editor::{Command, Document, EditorHost};
        crate::editor_document::with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source("(insert \"x\") (set-buffer-modified-p nil)").map_err(anyhow::Error::msg)?;
            for _ in 0..3 {
                host.execute(Command::Undo)?;
                let state = host.snapshot()?;
                assert_eq!(state.buffers[0].document.to_text(), "");
                assert!(!state.buffers[0].modified);
                host.execute(Command::Redo)?;
                let state = host.snapshot()?;
                assert_eq!(state.buffers[0].document.to_text(), "x");
                assert!(!state.buffers[0].modified);
            }
            Ok(())
        })
    }
    #[test]
    fn modified_history_recaptures_force_state_between_cycles() -> anyhow::Result<()> {
        use rune_editor::{Command, EditorHost};
        crate::editor_document::with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source("(set-buffer-modified-p t) (insert \"x\")").map_err(anyhow::Error::msg)?;
            host.execute(Command::Undo)?;
            assert!(host.snapshot()?.buffers[0].modified);
            host.evaluate_source("(set-buffer-modified-p nil)").map_err(anyhow::Error::msg)?;
            host.execute(Command::Redo)?;
            assert!(host.snapshot()?.buffers[0].modified);
            host.execute(Command::Undo)?;
            assert!(!host.snapshot()?.buffers[0].modified);
            Ok(())
        })
    }
    #[test]
    fn metadata_history_restores_dirty_flags_with_properties() -> anyhow::Result<()> {
        use rune_editor::{Command, EditorHost};
        crate::editor_document::with_lisp_runtime_owner(80, 24, |mut host| {
            host.evaluate_source("(insert \"x\") (set-buffer-modified-p nil) (put-text-property 1 2 'face 'bold)").map_err(anyhow::Error::msg)?;
            assert!(host.snapshot()?.buffers[0].modified);
            host.execute(Command::Undo)?;
            assert!(!host.snapshot()?.buffers[0].modified);
            host.evaluate_source("(if (get-text-property 1 'face) (error \"property undo failed\"))").map_err(anyhow::Error::msg)?;
            host.execute(Command::Redo)?;
            assert!(host.snapshot()?.buffers[0].modified);
            host.evaluate_source("(if (eq (get-text-property 1 'face) 'bold) nil (error \"property redo failed\"))").map_err(anyhow::Error::msg)?;
            Ok(())
        })
    }
    #[test]
    fn modified_flags_accept_nonnil_zero_and_retain_killed_buffer_state() {
        crate::assert_elprop!(r#"(progn (set-buffer-modified-p nil)
          (list (set-buffer-modified-p 0) (buffer-modified-p)
                (set-buffer-modified-p 'anything) (buffer-modified-p)))"#);
        crate::assert_elprop!(r#"(save-current-buffer
          (let ((clean (get-buffer-create "modified-dead-clean"))
                (dirty (get-buffer-create "modified-dead-dirty")))
            (set-buffer clean) (set-buffer-modified-p nil)
            (set-buffer dirty) (insert "é🙂") (set-buffer-modified-p t)
            (kill-buffer clean) (kill-buffer dirty) (garbage-collect)
            (let ((replacement (get-buffer-create "modified-dead-dirty")))
              (set-buffer replacement) (set-buffer-modified-p nil)
              (list (buffer-modified-p clean) (buffer-modified-p dirty)
                    (buffer-live-p clean) (buffer-live-p dirty)
                    (buffer-modified-p replacement) (buffer-modified-p dirty)))))"#);
    }
    #[test]
    fn modified_flag_publication_does_not_create_text_revision_or_undo() -> anyhow::Result<()> {
        use super::*;
        use crate::core::gc::RootSet;
        use rune_core::macros::root;
        use rune_editor::{Command, EditorHost};
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new(cx)?), cx);
        let g = env.editor_gateway();
        let before = env.buffer_read(cx)?;
        let mut native = g.host(cx)?;
        let sequence = native.snapshot()?.sequence;
        set_buffer_modified_p(crate::core::object::TRUE, env, cx)?;
        assert!(buffer_modified_p(None, env, cx)?);
        assert!(native.snapshot()?.sequence > sequence);
        assert_eq!(env.buffer_read(cx)?.epoch, before.epoch);
        let changed = native.snapshot()?.sequence;
        set_buffer_modified_p(crate::core::object::TRUE, env, cx)?;
        assert_eq!(native.snapshot()?.sequence, changed);
        assert!(matches!(native.execute(Command::Undo), Err(rune_editor::EditorError::NoUndo)));
        set_buffer_modified_p(NIL, env, cx)?;
        assert!(!buffer_modified_p(None, env, cx)?);
        assert_eq!(env.buffer_read(cx)?.epoch, before.epoch);
        Ok(())
    }
    #[test]
    fn modified_primitives_match_gnu_flags_returns_and_explicit_buffers() {
        crate::assert_elprop!(r#"(progn (set-buffer-modified-p nil)
          (list (buffer-modified-p) (set-buffer-modified-p t) (buffer-modified-p)
                (set-buffer-modified-p nil) (buffer-modified-p)
                (progn (insert "x") (buffer-modified-p))
                (set-buffer-modified-p nil) (buffer-modified-p)))"#);
        crate::assert_elprop!(r#"(save-current-buffer
          (let ((a (get-buffer-create "modified-oracle-a"))
                (b (get-buffer-create "modified-oracle-b")))
            (set-buffer a) (set-buffer-modified-p t)
            (set-buffer b) (set-buffer-modified-p nil)
            (list (buffer-modified-p a) (buffer-modified-p b) (buffer-modified-p))))"#);
    }
}
