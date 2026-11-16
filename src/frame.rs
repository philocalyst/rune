//! Real owner-local headless frame primitives. No OS window or GUI capability.
use crate::core::{
    gc::Context,
    object::{Gc, LispFrame, NIL, Object, ObjectType, TRUE},
};
use anyhow::{Result, anyhow};
use rune_macros::defun;

// GNU Emacs 30.1 frame.c::syms_of_frame, non-HAVE_X_WINDOWS branch.
// These are per-frame identities excluded from cloning/session persistence,
// not configurable defaults. This logical Terminal backend has no X window IDs.
defvar!(FRAME_INTERNAL_PARAMETERS, crate::frame::initial_internal_parameters(cx));
pub(crate) fn initial_internal_parameters<'a>(cx: &'a Context) -> Object<'a> {
    let names = ["name", "parent-id", "window-id"].map(|name| Object::from(crate::lread::intern(name, cx)));
    crate::fns::slice_into_list(&names, None, cx)
}

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
/// Number of color cells supplied by this backend. A logical terminal without
/// an attached terminal color driver has none, matching GNU batch terminals.
#[defun]
fn display_color_cells(frame: Option<Object>, cx: &Context) -> Result<i64> {
    let frame = live_frame(frame, cx)?;
    match frame.backend() {
        crate::core::object::FrameBackend::Terminal => Ok(0),
    }
}
/// A terminal's canonical character cell has unit width/height. This is
/// terminal coordinate semantics, not measured font pixels or native rendering.
/// Owner validation intentionally rejects foreign/deleted/unavailable frames;
/// GNU's no-window-system build skips argument checks, so those rejection cases
/// are a local fail-closed contract, not cross-build error parity.
fn terminal_character_cell(frame: Option<Object>, cx: &Context) -> Result<i64> {
    let frame = live_frame(frame, cx)?;
    match frame.backend() {
        crate::core::object::FrameBackend::Terminal => Ok(1),
    }
}
#[defun]
fn frame_char_width(frame: Option<Object>, cx: &Context) -> Result<i64> {
    terminal_character_cell(frame, cx)
}
#[defun]
fn frame_char_height(frame: Option<Object>, cx: &Context) -> Result<i64> {
    terminal_character_cell(frame, cx)
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
/// Snapshot the parameters implemented by the owner-local Terminal backend.
/// Geometry is read once from the authoritative Session tree. Returned cons
/// cells are fresh; mutating the alist does not mutate frame parameters.
/// This backend currently has no arbitrary parameter storage or GUI parameters.
#[defun]
fn frame_parameters<'a>(
    frame: Option<Object<'a>>,
    env: &crate::core::gc::Rt<crate::core::env::Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let frame = live_frame(frame, cx)?;
    let (width, height) = env.editor_gateway().frame_geometry(cx)?;
    let entries: [(&str, Object<'a>); 10] = [
        ("window-system", NIL),
        ("reverse", NIL),
        ("display-type", crate::lread::intern("mono", cx).into()),
        ("background-mode", crate::lread::intern("dark", cx).into()),
        ("font", cx.add("tty")),
        ("foreground-color", cx.add("unspecified-fg")),
        ("background-color", cx.add("unspecified-bg")),
        ("name", cx.add(format!("rune-terminal-{}", frame.id()))),
        ("width", i64::from(width).into()),
        ("height", i64::from(height).into()),
    ];
    let pairs: Vec<Object<'a>> = entries.into_iter().map(|(name, value)| {
        crate::core::cons::Cons::new(crate::lread::intern(name, cx), value, cx).into()
    }).collect();
    Ok(crate::fns::slice_into_list(&pairs, None, cx))
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_color_cells_match_gnu_batch_and_validate_frame() {
        crate::assert_elprop!("(list (display-color-cells) (display-color-cells nil) (display-color-cells (selected-frame)))");
        let roots = crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(&roots);
        assert!(super::display_color_cells(Some(42_i64.into()), cx).is_err());
        let frame = cx.selected_frame().unwrap();
        frame.delete(cx).unwrap();
        assert!(super::display_color_cells(Some(frame.into()), cx).is_err());
        assert!(super::display_color_cells(None, cx).is_err());
    }
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

#[cfg(test)]
mod internal_parameter_tests {
    #[test]
    fn real_non_x_identity_list_is_mutable_rooted_and_filters_frameset() {
        crate::editor_document::with_lisp_runtime_owner(80,24,|mut host| {
            host.evaluate_source(r#"
(setq frame-test-original frame-internal-parameters)
(if (equal frame-internal-parameters '(name parent-id window-id)) nil (signal 'error '("wrong identity list")))
(if (equal (mapcar (function (lambda (p) (cons p :never))) frame-internal-parameters) '((name . :never) (parent-id . :never) (window-id . :never))) nil (signal 'error '("wrong session filter")))
(let ((frame-internal-parameters '(temporary))) (if (equal frame-internal-parameters '(temporary)) nil (signal 'error '("dynamic binding failed"))))
(if (eq frame-test-original frame-internal-parameters) nil (signal 'error '("dynamic restore lost identity")))
(setcar frame-internal-parameters 'changed)
(garbage-collect)
(if (eq (car frame-test-original) 'changed) nil (signal 'error '("mutable identity lost across GC")))
(setcar frame-internal-parameters 'name)
"#).map_err(anyhow::Error::msg)?;
            Ok(())
        }).unwrap();
    }
}

#[cfg(test)]
mod terminal_cell_metric_tests {
    #[test]
    fn terminal_character_cells_match_gnu_valid_frame_calls() {
        crate::assert_elprop!(
            "(list (frame-char-width) (frame-char-width nil) (frame-char-width (selected-frame)) (frame-char-height) (frame-char-height nil) (frame-char-height (selected-frame)))"
        );
    }

    #[test]
    fn terminal_character_cells_validate_owner_frame_and_keep_identity() {
        use crate::core::{gc::{Context, RootSet}, object::NIL};
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let frame = cx.selected_frame().unwrap();
        let identity = frame.id();
        assert_eq!(super::frame_char_width(Some(frame.into()), cx).unwrap(), 1);
        assert_eq!(super::frame_char_height(Some(frame.into()), cx).unwrap(), 1);
        assert_eq!(super::frame_char_width(Some(NIL), cx).unwrap(), 1);
        assert_eq!(super::frame_char_height(None, cx).unwrap(), 1);
        assert_eq!(cx.selected_frame().unwrap().id(), identity);
        assert!(super::frame_char_width(Some(42_i64.into()), cx).is_err());
        assert!(super::frame_char_height(Some(cx.add("named-display")), cx).is_err());
        frame.delete(cx).unwrap();
        assert!(super::frame_char_width(Some(frame.into()), cx).is_err());
        assert!(super::frame_char_height(Some(frame.into()), cx).is_err());
        assert!(super::frame_char_width(None, cx).is_err());
        assert!(super::frame_char_height(None, cx).is_err());
    }
}

#[cfg(test)]
mod frame_parameter_snapshot_tests {
    #[test]
    fn terminal_snapshot_uses_real_owner_dimensions_and_fresh_pairs() {
        for (width, height) in [(80,24), (125,37)] {
            crate::editor_document::with_lisp_runtime_owner(width,height,|mut host| {
                host.evaluate_source(&format!(r#"
(let* ((f (selected-frame)) (p (frame-parameters f)) (q (frame-parameters nil)))
  (if (equal (cdr (assq 'width p)) {width}) nil (signal 'error '("wrong width")))
  (if (equal (cdr (assq 'height p)) {height}) nil (signal 'error '("wrong height")))
  (if (equal (cdr (assq 'width p)) (frame-parameter f 'width)) nil (signal 'error '("width mismatch")))
  (if (equal (cdr (assq 'height p)) (frame-parameter f 'height)) nil (signal 'error '("height mismatch")))
  (setcdr (assq 'width p) 999)
  (if (equal (cdr (assq 'width q)) {width}) nil (signal 'error '("shared snapshot pair")))
  (if (equal (frame-parameter f 'width) {width}) nil (signal 'error '("snapshot mutated owner")))
  (if (eq f (selected-frame)) nil (signal 'error '("frame identity changed"))))
"#)).map_err(anyhow::Error::msg)?;
                assert!(host.evaluate_source("(frame-parameters 42)").is_err());
                assert!(host.evaluate_source("(frame-parameters \"named-display\")").is_err());
                Ok(())
            }).unwrap();
        }
    }
    #[test]
    fn terminal_snapshot_dimension_relationship_matches_gnu() {
        crate::assert_elprop!(
            "(let* ((f (selected-frame)) (p (frame-parameters f))) (list (equal (cdr (assq 'width p)) (frame-parameter f 'width)) (equal (cdr (assq 'height p)) (frame-parameter f 'height))))"
        );
    }
}
