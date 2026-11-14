//! Real owner-local headless frame primitives. No OS window or GUI capability.
use crate::core::{
    gc::Context,
    object::{Gc, LispFrame, NIL, Object, ObjectType, TRUE},
};
use anyhow::{Result, anyhow};
use rune_macros::defun;

pub(crate) fn live_frame<'a>(
    frame: Option<Object<'a>>,
    cx: &'a Context,
) -> Result<Gc<&'a LispFrame>> {
    let frame = match frame.filter(|x| !x.is_nil()) {
        Some(value) => Gc::<&LispFrame>::try_from(value)?,
        None => cx.selected_frame()?,
    };
    frame.validate(cx)?;
    Ok(frame)
}
#[defun]
fn selected_frame<'a>(cx: &'a Context) -> Result<Gc<&'a LispFrame>> {
    cx.selected_frame()
}
#[defun]
fn frame_list<'a>(cx: &'a Context) -> Result<Object<'a>> {
    let frames: Vec<Object> = cx.live_frames()?.into_iter().map(Into::into).collect();
    Ok(crate::fns::slice_into_list(&frames, None, cx))
}
#[defun]
fn framep(object: Object) -> Object {
    if matches!(object.untag(), ObjectType::Frame(_)) { TRUE } else { NIL }
}
#[defun]
fn frame_live_p<'a>(object: Object<'a>, cx: &'a Context) -> Object<'a> {
    match object.untag() {
        ObjectType::Frame(frame) if frame.validate(cx).is_ok() => TRUE,
        _ => NIL,
    }
}
#[defun]
fn display_graphic_p(frame: Option<Object>, cx: &Context) -> Result<bool> {
    if frame.is_some_and(|x| matches!(x.untag(), ObjectType::String(_) | ObjectType::ByteString(_)))
    {
        return Err(anyhow!("named displays are unsupported by headless frame backend"));
    }
    let _frame = live_frame(frame, cx)?;
    Ok(false)
}
/// Return the window-system symbol for a validated owner frame. The current
/// logical terminal has no window-system connection, so its genuine value is nil.
#[defun]
fn window_system<'a>(frame: Option<Object<'a>>, cx: &'a Context) -> Result<Object<'a>> {
    let frame = live_frame(frame, cx)?;
    match frame.backend() {
        crate::core::object::FrameBackend::Terminal => Ok(NIL),
    }
}
/// Whether this frame can faithfully realize the requested face attributes.
/// The only current backend is a logical headless terminal, with no terminal
/// capability driver or graphical renderer attached. GNU's batch terminal
/// likewise reports no supported face attributes, including an empty request.
#[defun]
fn display_supports_face_attributes_p(
    _attributes: Object,
    frame: Option<Object>,
    cx: &Context,
) -> Result<bool> {
    let frame = live_frame(frame, cx)?;
    match frame.backend() {
        crate::core::object::FrameBackend::Terminal => Ok(false),
    }
}
/// Read-only parameters of the real owner logical terminal. Geometry comes
/// from its Session window tree; no physical display size is invented.
#[defun]
fn frame_parameter<'a>(
    frame: Object<'a>,
    parameter: crate::core::object::Symbol,
    env: &crate::core::gc::Rt<crate::core::env::Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let frame = live_frame(Some(frame), cx)?;
    if !parameter.interned() {
        return Ok(NIL);
    }
    Ok(match parameter.name() {
        "window-system" | "reverse" => NIL,
        "display-type" => crate::lread::intern("mono", cx).into(),
        "background-mode" => crate::lread::intern("dark", cx).into(),
        "font" => cx.add("tty"),
        "foreground-color" => cx.add("unspecified-fg"),
        "background-color" => cx.add("unspecified-bg"),
        "name" => cx.add(format!("rune-terminal-{}", frame.id())),
        "width" => i64::from(env.editor_gateway().frame_geometry(cx)?.0).into(),
        "height" => i64::from(env.editor_gateway().frame_geometry(cx)?.1).into(),
        _ => NIL,
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn terminal_window_system_matches_gnu_batch() {
        crate::assert_elprop!("(list (window-system) (window-system (selected-frame)))");
        let roots = crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(&roots);
        assert!(super::window_system(Some(42_i64.into()), cx).is_err());
        let frame = cx.selected_frame().unwrap();
        frame.delete(cx).unwrap();
        assert!(super::window_system(Some(frame.into()), cx).is_err());
        assert!(super::window_system(None, cx).is_err());
    }
    #[test]
    fn headless_face_capabilities_match_gnu_batch() {
        crate::assert_elprop!(
            "(list (display-supports-face-attributes-p nil) (display-supports-face-attributes-p '(:weight bold)) (display-supports-face-attributes-p '(:slant italic) (selected-frame)) (display-supports-face-attributes-p 42))"
        );
    }
    #[test]
    fn face_capability_query_rejects_non_frame() {
        let roots = crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(&roots);
        {
            assert!(super::display_supports_face_attributes_p(crate::core::object::NIL, Some(42_i64.into()), cx).is_err());
        }
    }
    #[test]
    fn headless_frame_identity_and_capabilities_match_gnu_batch() {
        crate::assert_elprop!(
            "(let ((f (selected-frame))) (list (framep f) (frame-live-p f) (eq f (selected-frame)) (eq f (car (frame-list))) (display-graphic-p f)))"
        );
    }
}
