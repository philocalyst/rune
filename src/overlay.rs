//! Owner-local Unicode overlays. Positions are real editor markers; properties
//! are ordinary traced fields of the Overlay object, not self-rooting leases.
use crate::core::{
    cons::Cons,
    env::{Env, intern},
    gc::{Context, Rt},
    object::{
        Gc, LispBuffer, LispOverlay, NIL, Object, ObjectType, OptionalFlag, OverlayAttachment,
    },
};
use crate::editor_document::OwnerBufferHandle;
use anyhow::{Result, anyhow, ensure};
use rune_editor::Bias;
use rune_macros::defun;

fn bias(advance: bool) -> Bias {
    if advance { Bias::Right } else { Bias::Left }
}
fn positions(beg: i64, end: i64, length: usize) -> (usize, usize) {
    let position = |p: i64| usize::try_from(p.max(1) - 1).unwrap_or(usize::MAX).min(length);
    let a = position(beg);
    let b = position(end);
    (a.min(b), a.max(b))
}
fn attachment(overlay: &LispOverlay, cx: &Context) -> Result<Option<OverlayAttachment>> {
    overlay.validate(cx)?;
    let attached = overlay.attachment(cx)?;
    if attached
        .as_ref()
        .is_some_and(|a| !a.liveness().is_live() || !a.owner_liveness().is_live())
    {
        overlay.set_attachment(None, cx)?;
        cx.unroot_overlay(overlay.id())?;
        return Ok(None);
    }
    Ok(attached)
}
fn range(overlay: &LispOverlay, cx: &Context) -> Result<Option<(OwnerBufferHandle, usize, usize)>> {
    let Some(a) = attachment(overlay, cx)? else {
        return Ok(None);
    };
    let Some((start, end)) =
        cx.editor_gateway()?.marker_pair_positions(a.buffer(), (a.start(), a.end()), cx)?
    else {
        return Err(anyhow!("Overlay marker pair is missing"));
    };
    // A front-advancing/rear-staying empty pair can cross on insertion. GNU
    // collapses it at END, rather than sorting and including the inserted text.
    Ok(Some((a.buffer(), start.min(end), end)))
}

#[defun]
fn overlayp(value: Object) -> bool {
    matches!(value.untag(), ObjectType::Overlay(_))
}

#[defun]
fn make_overlay<'ob>(
    beg: i64,
    end: i64,
    buffer: Option<Gc<&LispBuffer>>,
    front_advance: OptionalFlag,
    rear_advance: OptionalFlag,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Gc<&'ob LispOverlay>> {
    let handle = buffer.map_or_else(|| env.current_buffer_handle(), |b| b.untag().handle());
    let gateway = env.editor_gateway();
    let read = gateway.read(handle, cx)?;
    let (start, end) = positions(beg, end, read.document.text().len_chars());
    let live = gateway.buffer_liveness(handle, cx)?;
    let object = LispOverlay::new(cx, front_advance.is_some(), rear_advance.is_some());
    let overlay = object.untag();
    // Root allocation and owner validation occur before marker publication.
    cx.root_overlay(overlay)?;
    let pair = match gateway.register_marker_pair(
        handle,
        start,
        bias(overlay.front_advance()),
        end,
        bias(overlay.rear_advance()),
        cx,
    ) {
        Ok(pair) => pair,
        Err(error) => {
            cx.unroot_overlay(overlay.id())?;
            return Err(error);
        }
    };
    overlay.set_attachment(
        Some(unsafe {
            // Pair and witness came from the same authenticated gateway operation;
            // no callback, collection, or owner mutation intervenes.
            OverlayAttachment::from_marker_pair(handle, pair.0, pair.1, live, cx)?
        }),
        cx,
    )?;
    Ok(object)
}

#[defun]
fn delete_overlay(overlay: Gc<&LispOverlay>, cx: &Context) -> Result<()> {
    let overlay = overlay.untag();
    if let Some(a) = attachment(overlay, cx)? {
        cx.editor_gateway()?.remove_marker_pair(a.buffer(), (a.start(), a.end()), cx)?;
        overlay.set_attachment(None, cx)?;
    }
    cx.unroot_overlay(overlay.id())?;
    Ok(())
}

#[defun]
fn move_overlay<'ob>(
    object: Gc<&'ob LispOverlay>,
    beg: i64,
    end: i64,
    buffer: Option<Gc<&LispBuffer>>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Gc<&'ob LispOverlay>> {
    let overlay = object.untag();
    let old = attachment(overlay, cx)?;
    // Unlike make-overlay, omitted BUFFER keeps an attached overlay's buffer.
    let target = buffer
        .map(|b| b.untag().handle())
        .or_else(|| old.as_ref().map(|a| a.buffer()))
        .unwrap_or_else(|| env.current_buffer_handle());
    let gateway = env.editor_gateway();
    let read = gateway.read(target, cx)?;
    let (start, end) = positions(beg, end, read.document.text().len_chars());
    let front = bias(overlay.front_advance());
    let rear = bias(overlay.rear_advance());
    let target_live = gateway.buffer_liveness(target, cx)?;
    let (pair, live) = match old {
        Some(a) if a.buffer() == target => {
            gateway.move_marker_pair(target, (a.start(), a.end()), start, front, end, rear, cx)?;
            ((a.start(), a.end()), target_live)
        }
        Some(a) => {
            let live = gateway.transfer_marker_pair(
                a.buffer(),
                target,
                (a.start(), a.end()),
                start,
                front,
                end,
                rear,
                cx,
            )?;
            ((a.start(), a.end()), live)
        }
        None => {
            cx.root_overlay(overlay)?;
            match gateway.register_marker_pair(target, start, front, end, rear, cx) {
                Ok(pair) => (pair, target_live),
                Err(error) => {
                    cx.unroot_overlay(overlay.id())?;
                    return Err(error);
                }
            }
        }
    };
    overlay.set_attachment(
        Some(unsafe {
            // Pair and witness came from the same authenticated gateway operation;
            // no callback, collection, or owner mutation intervenes.
            OverlayAttachment::from_marker_pair(target, pair.0, pair.1, live, cx)?
        }),
        cx,
    )?;
    Ok(object)
}

#[defun]
fn overlay_start(overlay: Gc<&LispOverlay>, cx: &Context) -> Result<Option<usize>> {
    Ok(range(overlay.untag(), cx)?.map(|(_, start, _)| start + 1))
}
#[defun]
fn overlay_end(overlay: Gc<&LispOverlay>, cx: &Context) -> Result<Option<usize>> {
    Ok(range(overlay.untag(), cx)?.map(|(_, _, end)| end + 1))
}
#[defun]
fn overlay_buffer<'ob>(overlay: Gc<&LispOverlay>, cx: &'ob Context) -> Result<Object<'ob>> {
    Ok(match attachment(overlay.untag(), cx)? {
        Some(a) => cx.add(LispBuffer::for_handle(a.buffer(), cx)?),
        None => NIL,
    })
}

fn property_pairs<'ob>(
    overlay: &LispOverlay,
    cx: &'ob Context,
) -> Result<Vec<(Object<'ob>, Object<'ob>)>> {
    let mut pairs = Vec::new();
    let mut tail = overlay.plist(cx)?;
    while !tail.is_nil() {
        ensure!(pairs.len() < 4096, "Overlay property list exceeds limit or is cyclic");
        let key = tail.cons().ok_or_else(|| anyhow!("Malformed overlay property list"))?;
        let value = key.cdr().cons().ok_or_else(|| anyhow!("Odd overlay property list"))?;
        pairs.push((cx.bind(key.car()), cx.bind(value.car())));
        tail = value.cdr();
    }
    Ok(pairs)
}

#[defun]
fn overlay_put<'ob>(
    overlay: Gc<&LispOverlay>,
    property: Object,
    value: Object<'ob>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let overlay = overlay.untag();
    overlay.validate(cx)?;
    // These properties require edit-time callbacks/evaporation, not just storage.
    if !value.is_nil() {
        for name in [
            "evaporate",
            "modification-hooks",
            "insert-in-front-hooks",
            "insert-behind-hooks",
        ] {
            ensure!(
                !crate::fns::eq(property, Object::from(intern(name, cx))),
                "Overlay edit hooks and evaporation are not implemented"
            );
        }
    }
    let mut entries = property_pairs(overlay, cx)?;
    if let Some((_, old)) = entries.iter_mut().find(|(key, _)| crate::fns::eq(*key, property)) {
        *old = value;
    } else {
        ensure!(entries.len() < 4096, "Overlay property limit exceeded");
        entries.insert(0, (property, value));
    }
    let mut plist = NIL;
    for (key, value) in entries.into_iter().rev() {
        plist = Cons::new(key, Cons::new(value, plist, cx), cx).into();
    }
    // SAFETY: values are current-Context primitive arguments or fields traced
    // from this validated overlay. Shared Context allocation cannot collect.
    unsafe {
        overlay.set_plist(plist, cx)?;
    }
    Ok(value)
}

#[defun]
fn overlay_get<'ob>(
    overlay: Gc<&LispOverlay>,
    property: Object,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let pairs = property_pairs(overlay.untag(), cx)?;
    if let Some((_, value)) = pairs.iter().find(|(key, _)| crate::fns::eq(*key, property)) {
        return Ok(*value);
    }
    let category = Object::from(intern("category", cx));
    if let Some((_, value)) = pairs.iter().find(|(key, _)| crate::fns::eq(*key, category)) {
        if let (ObjectType::Symbol(category), ObjectType::Symbol(property)) =
            (value.untag(), property.untag())
        {
            return Ok(crate::data::get(category, property, env, cx));
        }
    }
    Ok(NIL)
}

#[defun]
fn overlay_properties<'ob>(overlay: Gc<&LispOverlay>, cx: &'ob Context) -> Result<Object<'ob>> {
    let mut list = NIL;
    for (key, value) in property_pairs(overlay.untag(), cx)?.into_iter().rev() {
        list = Cons::new(key, Cons::new(value, list, cx), cx).into();
    }
    Ok(list)
}

#[defun]
fn overlays_at<'ob>(
    position: i64,
    sorted: OptionalFlag,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    ensure!(sorted.is_none(), "Priority-sorted overlays-at is not implemented");
    let read = env.buffer_read(cx)?;
    let Ok(position) = usize::try_from(position.saturating_sub(1)) else {
        return Ok(NIL);
    };
    if position > read.document.text().len_chars() {
        return Ok(NIL);
    }
    let roots = cx.attached_overlays(read.handle)?;
    let mut result = NIL;
    for root in roots {
        let object: Gc<&LispOverlay> = root.bind(cx)?.try_into()?;
        if let Some((_, start, end)) = range(object.untag(), cx)? {
            if start <= position && position < end {
                result = Cons::new(object, result, cx).into();
            }
        }
    }
    Ok(result)
}

#[defun]
fn overlays_in<'ob>(beg: i64, end: i64, env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    if beg > end {
        return Ok(NIL);
    }
    let read = env.buffer_read(cx)?;
    let length = read.document.text().len_chars();
    if end < 1 || i128::from(beg) > length as i128 + 1 {
        return Ok(NIL);
    }
    let (beg, end) = positions(beg, end, length);
    let roots = cx.attached_overlays(read.handle)?;
    let mut result = NIL;
    for root in roots {
        let object: Gc<&LispOverlay> = root.bind(cx)?.try_into()?;
        if let Some((_, start, finish)) = range(object.untag(), cx)? {
            let intersects = if start == finish {
                (beg <= start && start < end) || (start == end && (beg == end || end == length))
            } else {
                start < end && finish > beg
            };
            if intersects {
                result = Cons::new(object, result, cx).into();
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::{rebind, root};
    use rune_editor::EditorHost;

    fn eval<'ob>(source: &str, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
        let form = crate::reader::read(source, cx)?.0;
        root!(form, cx);
        Ok(rebind!(crate::interpreter::eval(form, None, env, cx)?, cx))
    }
    #[test]
    fn org_overlay_initialization_is_real_and_detached_properties_survive_gc() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let before = env.buffer_read(cx)?.epoch;
        let source = r#"(progn
          (defvar rune-org-overlay (make-overlay 1 1))
          (overlay-put rune-org-overlay 'face 'org-agenda-restriction-lock)
          (overlay-put rune-org-overlay 'help-echo (concat "retained" " value"))
          (delete-overlay rune-org-overlay)
          (list (overlayp rune-org-overlay) (type-of rune-org-overlay)
                (overlay-buffer rune-org-overlay) (overlay-start rune-org-overlay)
                (overlay-end rune-org-overlay)))"#;
        assert_eq!(eval(source, env, cx)?.to_string(), "(t overlay nil nil nil)");
        cx.garbage_collect(true);
        assert_eq!(eval("(list (overlay-get rune-org-overlay 'face) (overlay-get rune-org-overlay 'help-echo))", env, cx)?.to_string(),
            "(org-agenda-restriction-lock \"retained value\")");
        assert_eq!(env.buffer_read(cx)?.epoch, before);
        assert!(!env.editor_gateway().modified(env.current_buffer_handle(), cx)?);
        Ok(())
    }
    #[test]
    fn unicode_endpoints_track_edits_and_history_without_resurrecting_deleted_overlay() -> Result<()>
    {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        assert_eq!(eval("(progn (insert \"λ🙂z\") (setq rune-ov (make-overlay 2 3)) (goto-char 2) (insert \"a\") (list (overlay-start rune-ov) (overlay-end rune-ov)))", env, cx)?.to_string(), "(2 4)");
        {
            env.editor_gateway().host(cx)?.execute(rune_editor::Command::Undo)?;
        }
        assert_eq!(
            eval("(list (overlay-start rune-ov) (overlay-end rune-ov))", env, cx)?.to_string(),
            "(2 3)"
        );
        {
            env.editor_gateway().host(cx)?.execute(rune_editor::Command::Redo)?;
        }
        assert_eq!(
            eval("(list (overlay-start rune-ov) (overlay-end rune-ov))", env, cx)?.to_string(),
            "(2 4)"
        );
        eval("(delete-overlay rune-ov)", env, cx)?;
        {
            env.editor_gateway().host(cx)?.execute(rune_editor::Command::Undo)?;
        }
        {
            env.editor_gateway().host(cx)?.execute(rune_editor::Command::Redo)?;
        }
        assert_eq!(
            eval(
                "(list (overlay-buffer rune-ov) (overlay-start rune-ov) (overlay-end rune-ov))",
                env,
                cx
            )?
            .to_string(),
            "(nil nil nil)"
        );
        Ok(())
    }
    #[test]
    fn empty_overlay_bias_crossover_and_property_spine_match_gnu() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let source = r#"(progn
          (setq a (make-overlay 1 1 nil nil nil) b (make-overlay 1 1 nil t nil)
                c (make-overlay 1 1 nil nil t) d (make-overlay 1 1 nil t t))
          (insert "abc")
          (list (overlay-start a) (overlay-end a) (overlay-start b) (overlay-end b)
                (overlay-start c) (overlay-end c) (overlay-start d) (overlay-end d)))"#;
        assert_eq!(eval(source, env, cx)?.to_string(), "(1 1 1 1 1 4 4 4)");
        let source = r#"(progn (put 'rune-ov-category 'face 'category-face)
          (overlay-put a 'category 'rune-ov-category)
          (setq category-result (overlay-get a 'face)) (overlay-put a 'face nil)
          (let ((copy (overlay-properties a))) (setcar copy 'changed))
          (list category-result (overlay-get a 'face) (overlay-properties a)))"#;
        assert_eq!(
            eval(source, env, cx)?.to_string(),
            "(category-face nil (face nil category rune-ov-category))"
        );
        Ok(())
    }
    #[test]
    fn move_keeps_original_buffer_by_default_and_kill_detaches_without_losing_values() -> Result<()>
    {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let source = r#"(progn (setq source (current-buffer) ov (make-overlay 1 1 nil t t))
          (overlay-put ov 'payload (concat "kept" " value"))
          (setq target (get-buffer-create "rune-overlay-target")) (set-buffer target)
          (move-overlay ov 1 1) (setq stayed (eq (overlay-buffer ov) source))
          (move-overlay ov 1 1 target)
          (list stayed (eq (overlay-buffer ov) target)))"#;
        assert_eq!(eval(source, env, cx)?.to_string(), "(t t)");
        eval("(kill-buffer target)", env, cx)?;
        cx.garbage_collect(true);
        assert_eq!(eval("(list (overlay-buffer ov) (overlay-start ov) (overlay-end ov) (overlay-get ov 'payload))", env, cx)?.to_string(),
            "(nil nil nil \"kept value\")");
        eval("(move-overlay ov 1 1)", env, cx)?;
        assert_eq!(eval("(eq (overlay-buffer ov) (current-buffer))", env, cx)?.to_string(), "t");
        Ok(())
    }
}
