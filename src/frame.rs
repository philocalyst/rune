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
    fn headless_frame_identity_and_capabilities_match_gnu_batch() {
        crate::assert_elprop!(
            "(let ((f (selected-frame))) (list (framep f) (frame-live-p f) (eq f (selected-frame)) (eq f (car (frame-list))) (display-graphic-p f)))"
        );
    }
}
