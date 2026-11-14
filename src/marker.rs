//! Owner-arena Lisp markers backed by the single authoritative editor registry.
//! No copied position cache and no mutable Lisp value is sent to a worker.
use crate::core::{
    env::Env,
    gc::{Context, Rt},
    object::{Gc, LispBuffer, LispMarker, TagType, Object, ObjectType, Symbol},
};
use anyhow::{Result, anyhow};
use rune_macros::defun;
use crate::editor_document::OwnerBufferHandle;
use rune_core::macros::list;
fn wrong_type(predicate: Symbol, value: Object, cx: &Context) -> anyhow::Error {
    crate::eval::EvalError::signal(crate::core::env::sym::WRONG_TYPE_ARGUMENT.into(), list![predicate, value; cx], cx).into()
}
fn expect_marker<'a>(value: Object<'a>, cx: &Context) -> Result<Gc<&'a LispMarker>> {
    let ObjectType::Marker(marker) = value.untag() else { return Err(wrong_type(crate::core::env::sym::MARKERP, value, cx)); };
    marker.validate(cx)?;
    Ok(marker.tag())
}

#[defun]
fn make_marker<'a>(cx: &'a Context) -> Result<Gc<&'a LispMarker>> {
    LispMarker::new(cx)
}

pub(crate) fn location(marker: &LispMarker, cx: &Context) -> Result<Option<(OwnerBufferHandle, usize)>> {
    marker.validate(cx)?;
    let Some(attached) = marker.attachment(cx)? else { return Ok(None) };
    if !attached.is_live() { return Ok(None); }
    let position = cx.editor_gateway()?.marker_position(attached.buffer(), attached.id(), cx)?
        .ok_or_else(|| anyhow!("Live Lisp marker registration is missing"))?;
    Ok(Some((attached.buffer(), position)))
}
#[defun]
pub(crate) fn marker_position(marker: Object, cx: &Context) -> Result<Option<usize>> {
    location(expect_marker(marker, cx)?.untag(), cx)?.map(|(_, position)|
        position.checked_add(1).ok_or_else(|| anyhow!("Marker position overflow"))).transpose()
}
#[defun]
fn marker_buffer<'a>(marker: Object, cx: &'a Context) -> Result<Option<Gc<&'a LispBuffer>>> {
    location(expect_marker(marker, cx)?.untag(), cx)?.map(|(buffer, _)| LispBuffer::for_handle(buffer, cx).map(TagType::tag)).transpose()
}

#[defun]
fn marker_insertion_type(marker: Object, cx: &Context) -> Result<bool> {
    Ok(expect_marker(marker, cx)?.untag().insertion_type())
}

#[defun]
fn mark_marker<'a>(env: &Rt<Env>, cx: &'a Context) -> Result<Gc<&'a LispMarker>> {
    let handle = env.current_buffer_handle();
    let liveness = env.editor_gateway().buffer_liveness(handle, cx)?;
    // This origin-buffer canonical root is separate from the marker's current
    // attachment target: moving a mark elsewhere must not retain old text.
    // SAFETY: the witness was issued for this exact authenticated owner handle
    // in this callback-free shared-Context turn.
    unsafe { cx.mark_marker(handle, liveness) }
}

fn detach(marker: &LispMarker, cx: &Context) -> Result<()> {
    marker.validate(cx)?;
    cx.editor_gateway()?.detach_marker(marker.lifetime(cx)?, cx)?;
    // SAFETY: owner preflight and the immediately preceding native operation
    // share cx with no callback/GC. The private cell exposes no borrow guards.
    unsafe { marker.install_attachment_prevalidated(None) };
    Ok(())
}
fn attach(marker: &LispMarker, target: OwnerBufferHandle, at: usize, advance: bool, cx: &Context) -> Result<()> {
    marker.validate(cx)?;
    let registration = cx.editor_gateway()?.replace_marker_attachment(marker.lifetime(cx)?, target, at,
        if advance { rune_editor::Bias::Right } else { rune_editor::Bias::Left }, cx)?;
    let crate::editor_document::RegisteredMarker { buffer, id, liveness } = registration;
    // SAFETY: these fields came from ONE successful authenticated native
    // transaction; preflight remains valid and no GC/callback can intervene.
    // Both following updates are native/infallible after that commit.
    unsafe {
        let attachment = crate::core::object::MarkerAttachment::from_registration(buffer, id, liveness, cx);
        marker.install_attachment_prevalidated(Some(attachment));
        marker.set_insertion_type_prevalidated(advance);
    }
    Ok(())
}
fn integer_index(position: i64, len: usize) -> usize {
    if position <= 1 { 0 } else { usize::try_from(position - 1).unwrap_or(usize::MAX).min(len) }
}

#[defun]
fn set_marker<'a>(marker: crate::core::object::Object<'a>, position: crate::core::object::Object,
    buffer: Option<crate::core::object::Object>, env: &Rt<Env>, cx: &'a Context) -> Result<Gc<&'a LispMarker>> {
    // GNU checks the explicit BUFFER type before MARKER or POSITION, even for
    // detachment. A known dead buffer detaches after MARKER validation and does
    // not inspect POSITION. Do not use eager typed macro arguments here.
    let target = match buffer.filter(|b| !b.is_nil()) {
        Some(value) => {
            let ObjectType::Buffer(buffer) = value.untag() else { return Err(wrong_type(crate::core::env::sym::BUFFERP, value, cx)); };
            buffer.handle()
        },
        None => env.buffer_read(cx)?.handle,
    };
    let marker = expect_marker(marker, cx)?;
    marker.untag().validate(cx)?;
    if !cx.editor_gateway()?.buffer_is_live(target, cx)? {
        detach(marker.untag(), cx)?;
        return Ok(marker);
    }
    use crate::core::object::ObjectType;
    let index = match position.untag() {
        ObjectType::Symbol(crate::core::env::sym::NIL) => None,
        ObjectType::Int(position) => {
            let len = cx.editor_gateway()?.read(target, cx)?.document.text().len_chars();
            Some(integer_index(position, len))
        },
        ObjectType::Marker(source) => location(source, cx)?.map(|(_, at)| at),
        _ => return Err(wrong_type(crate::core::env::sym::INTEGER_OR_MARKER_P, position, cx)),
    };
    match index {
        None => detach(marker.untag(), cx)?,
        Some(at) => {
            let len = cx.editor_gateway()?.read(target, cx)?.document.text().len_chars();
            attach(marker.untag(), target, at.min(len), marker.untag().insertion_type(), cx)?;
        },
    }
    Ok(marker)
}

#[defun]
fn copy_marker<'a>(position: crate::core::object::Object, insertion_type: Option<crate::core::object::Object>,
    env: &Rt<Env>, cx: &'a Context) -> Result<Gc<&'a LispMarker>> {
    use crate::core::object::ObjectType;
    let target = match position.untag() {
        ObjectType::Symbol(crate::core::env::sym::NIL) => None,
        ObjectType::Int(position) => {
            let read = env.buffer_read(cx)?;
            Some((read.handle, integer_index(position, read.document.text().len_chars())))
        },
        ObjectType::Marker(source) => location(source, cx)?,
        _ => return Err(wrong_type(crate::core::env::sym::INTEGER_OR_MARKER_P, position, cx)),
    };
    let advance = insertion_type.is_some_and(|value| !value.is_nil());
    let marker = LispMarker::new(cx)?;
    match target {
        Some((buffer, at)) => attach(marker.untag(), buffer, at, advance, cx)?,
        None => marker.untag().set_insertion_type_native(advance, cx)?,
    }
    Ok(marker)
}

#[defun]
fn set_marker_insertion_type<'a>(marker: Object, insertion_type: crate::core::object::Object<'a>, cx: &'a Context)
    -> Result<crate::core::object::Object<'a>> {
    let marker = expect_marker(marker, cx)?.untag();
    let advance = !insertion_type.is_nil();
    match location(marker, cx)? {
        Some((buffer, at)) => attach(marker, buffer, at, advance, cx)?,
        None => marker.set_insertion_type_native(advance, cx)?,
    }
    // GNU returns the caller's original truthy value, not canonical t.
    Ok(insertion_type)
}

pub(crate) fn point_marker<'a>(env: &Rt<Env>, cx: &'a Context) -> Result<Gc<&'a LispMarker>> {
    let read = env.buffer_read(cx)?;
    let marker = LispMarker::new(cx)?;
    attach(marker.untag(), read.handle, read.point, false, cx)?;
    Ok(marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{gc::RootSet, object::{NIL, IntoObject}};
    use rune_core::macros::{root, list};

    fn assert_marker_condition_form(form: &str) {
        crate::library::elprop::start_emacs().unwrap();
        let expected=crate::library::elprop::eval(form).unwrap();
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        crate::core::env::sym::init_symbols();
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        // The generic elprop fixture omits generated startup variables; this
        // test needs the production condition ancestry for typed signals.
        env.initialize_condition_metadata(cx);
        let form=crate::reader::read(form,cx).unwrap().0;root!(form,cx);
        let value=crate::interpreter::eval(form,None,env,cx).unwrap();
        assert_eq!(format!("{value}"),expected);
    }
    #[test]
    fn marker_identity_detachment_and_bias_match_gnu() {
        crate::assert_elprop!(r#"(let* ((m (make-marker)) (c (copy-marker nil t)))
          (list (markerp m) (numberp m) (integerp m) (number-or-marker-p m)
                (integer-or-marker-p m) (marker-position m) (marker-buffer m)
                (marker-insertion-type m) (eq m c) (equal m c)
                (eq (mark-marker) (mark-marker)) (marker-position (mark-marker))
                (set-marker-insertion-type m 7) (marker-insertion-type m)
                (set-marker-insertion-type m nil) (marker-insertion-type m)))"#);
    }
    #[test]
    fn unicode_marker_bias_and_nested_comparators_match_gnu() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert "ab")
          (let* ((l (copy-marker 2)) (r (copy-marker 2 t)) (copy (copy-marker r))
                 (a (list l)) (b (list copy)) (alist (list (cons r 42))))
            (let ((before (list (equal l r) (equal a b) (eq l copy)
                                (marker-insertion-type copy)
                                (eq (car (member copy a)) l)
                                (cdr (assoc copy alist))
                                (car (rassoc copy (list (cons 42 r)))))))
              (goto-char 2) (insert "λ🙂")
              (list before (marker-position l) (marker-position r)
                    (marker-position copy) (equal l r)
                    (marker-position (point-marker))
                    (eq (point-marker) (point-marker))))))"#);
    }
    #[test]
    fn marker_transfer_and_dead_buffer_order_match_gnu() {
        assert_marker_condition_form(r#"(let ((a (get-buffer-create " *rune-marker-oracle-a*"))
                                    (b (get-buffer-create " *rune-marker-oracle-b*"))
                                    (m (make-marker)) (source (make-marker)))
          (set-buffer a) (erase-buffer) (insert "abcd") (set-marker source 4)
          (set-buffer b) (erase-buffer) (insert "x") (set-marker m source)
          (let ((before (list (marker-position m) (eq (marker-buffer m) b)
                              (marker-position (copy-marker source)))))
            (kill-buffer a)
            (set-marker m "bad-position-skipped-for-dead-buffer" a)
            (list before (marker-position m) (marker-buffer m)
                  (condition-case e (set-marker 42 "bad" 43) (error (list (car e) (car (cdr e)) (car (cdr (cdr e)))))))))"#);
    }
    #[test]
    fn marker_bad_position_and_cyclic_wrong_type_keep_original_identity() {
        assert_marker_condition_form(r#"(let ((m (make-marker)) (v (vector nil)))
          (aset v 0 v)
          (list (condition-case e (marker-position v)
                  (error (list (car e) (car (cdr e)) (eq (car (cdr (cdr e))) v))))
                (condition-case e (set-marker m "bad")
                  (error (list (car e) (car (cdr e)) (car (cdr (cdr e))))))))"#);
    }
    #[test]
    fn real_marker_gc_and_live_history_keep_one_authoritative_position() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        crate::core::env::sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        let read=env.buffer_read(cx).unwrap();
        env.editor_gateway().splice(read.handle,read.epoch,0..0,"aλ🙂z",cx).unwrap();
        let marker=copy_marker(3.into(),Some(true.into()),env,cx).unwrap();
        root!(marker,cx);
        for _ in 0..3 {cx.garbage_collect(true);assert_eq!(marker_position(marker.bind(cx).into(),cx).unwrap(),Some(3));}
        let read=env.buffer_read(cx).unwrap();
        env.editor_gateway().splice(read.handle,read.epoch,2..2,"x",cx).unwrap();
        assert_eq!(marker_position(marker.bind(cx).into(),cx).unwrap(),Some(4));
        {use rune_editor::EditorHost;let mut host=env.editor_gateway().host(cx).unwrap();host.execute(rune_editor::Command::Undo).unwrap();}
        cx.garbage_collect(true);
        assert_eq!(marker_position(marker.bind(cx).into(),cx).unwrap(),Some(3));
        {use rune_editor::EditorHost;let mut host=env.editor_gateway().host(cx).unwrap();host.execute(rune_editor::Command::Redo).unwrap();}
        assert_eq!(marker_position(marker.bind(cx).into(),cx).unwrap(),Some(4));
    }
    #[test]
    fn contextual_equality_is_used_by_real_bytecode_equal_and_member() {
        use crate::{bytecode::{self},core::{env::CallFrame,object::{ByteString,LispVec}}};
        let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::core::env::sym::init_symbols();
        root!(env,init(Env::new(cx).unwrap()),cx);
        let a=copy_marker(1.into(),None,env,cx).unwrap();let b=copy_marker(1.into(),Some(true.into()),env,cx).unwrap();
        let va=cx.add(vec![Object::from(a)]);let vb=cx.add(vec![Object::from(b)]);
        let constants: &LispVec=vec![va,vb].into_obj(cx).untag();
        let codes: &ByteString=vec![192u8,193,154,135].into_obj(cx).untag();
        let f=crate::alloc::make_byte_code(0,codes,constants,2,None,None,&[],cx).unwrap();root!(f,cx);
        {let mut frame=CallFrame::new(env);frame.finalize_arguments().unwrap();assert_eq!(bytecode::call(f,0,"nested-marker-equal",&mut frame,cx).unwrap(),Object::from(true));}
        let a=make_marker(cx).unwrap();let b=make_marker(cx).unwrap();let tail=list![b;cx];
        let constants: &LispVec=vec![a.into(),tail].into_obj(cx).untag();
        let codes: &ByteString=vec![192u8,193,157,135].into_obj(cx).untag();
        let f=crate::alloc::make_byte_code(0,codes,constants,2,None,None,&[],cx).unwrap();root!(f,cx);
        {let mut frame=CallFrame::new(env);frame.finalize_arguments().unwrap();assert!(!bytecode::call(f,0,"detached-marker-member",&mut frame,cx).unwrap().is_nil());}
    }
    #[test]
    fn actual_lisp_hash_wrappers_reject_marker_keys_before_lookup_or_mutation() {
        crate::interpreter::assert_lisp(r#"(let ((m (make-marker)) (h (make-hash-table)))
          (puthash "plain" m h)
          (list (condition-case nil (puthash m 7 h) (error t))
                (condition-case nil (gethash (list m) h) (error t))
                (condition-case nil (remhash m h) (error t))
                (eq (gethash "plain" h) m)))"#, "(t t t t)");
    }
    #[test]
    fn contextual_equality_hash_barriers_and_cycles_fail_closed() {
        let roots=RootSet::default();let cx=&Context::new(&roots);
        let marker=make_marker(cx).unwrap();
        assert!(crate::contextual_equal::reject_marker_hash_key(marker.into()).is_err());
        assert!(crate::contextual_equal::reject_marker_hash_key(list![marker;cx]).is_err());
        let a=crate::core::cons::Cons::new(NIL,NIL,cx);a.set_cdr(a.into()).unwrap();
        let b=crate::core::cons::Cons::new(NIL,NIL,cx);b.set_cdr(b.into()).unwrap();
        assert!(crate::contextual_equal::equal(a.into(),b.into(),cx).is_err());
        assert!(crate::contextual_equal::equal(a.into(),a.into(),cx).unwrap());
    }
}

#[cfg(test)]
mod marker_history_identity_regression {
    use super::*;
    use crate::core::{gc::RootSet,object::NIL};
    use rune_core::macros::root;
    enum Action { SameAnchor, Away, AwayBack, DetachReattach, Detached, TransferBack, Rebias }
    fn check(action:Action, expected:Option<usize>) {
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        root!(env,init(Env::new(cx).unwrap()),cx);
        let read=env.buffer_read(cx).unwrap();
        env.editor_gateway().splice(read.handle,read.epoch,0..0,"abcde",cx).unwrap();
        let marker=make_marker(cx).unwrap();root!(marker,cx);
        set_marker(marker.bind(cx).into(),3.into(),None,env,cx).unwrap();
        let read=env.buffer_read(cx).unwrap();
        env.editor_gateway().splice(read.handle,read.epoch,1..4,"",cx).unwrap();
        assert_eq!(marker_position(marker.bind(cx).into(),cx).unwrap(),Some(2));
        match action {
            Action::SameAnchor => {set_marker(marker.bind(cx).into(),2.into(),None,env,cx).unwrap();},
            Action::Away => {set_marker(marker.bind(cx).into(),1.into(),None,env,cx).unwrap();},
            Action::AwayBack => {
                set_marker(marker.bind(cx).into(),1.into(),None,env,cx).unwrap();
                set_marker(marker.bind(cx).into(),2.into(),None,env,cx).unwrap();
            },
            Action::DetachReattach => {
                set_marker(marker.bind(cx).into(),NIL,None,env,cx).unwrap();
                set_marker(marker.bind(cx).into(),2.into(),None,env,cx).unwrap();
            },
            Action::Detached => {set_marker(marker.bind(cx).into(),NIL,None,env,cx).unwrap();},
            Action::TransferBack => {
                let other=env.editor_gateway().create("marker-history-other","xy",cx).unwrap();
                let buffer=LispBuffer::for_handle(other,cx).unwrap().tag();
                set_marker(marker.bind(cx).into(),1.into(),Some(buffer.into()),env,cx).unwrap();
                set_marker(marker.bind(cx).into(),2.into(),None,env,cx).unwrap();
            },
            Action::Rebias => {set_marker_insertion_type(marker.bind(cx).into(),true.into(),cx).unwrap();},
        }
        {use rune_editor::EditorHost;let mut host=env.editor_gateway().host(cx).unwrap();host.execute(rune_editor::Command::Undo).unwrap();}
        assert_eq!(env.buffer_read(cx).unwrap().document.text().to_string(),"abcde");
        assert_eq!(marker_position(marker.bind(cx).into(),cx).unwrap(),expected);
    }
    #[test] fn same_anchor_reset_preserves_gnu_deletion_offset(){check(Action::SameAnchor,Some(3));}
    #[test] fn moved_away_marker_does_not_get_historical_offset(){check(Action::Away,Some(1));}
    #[test] fn moved_away_and_back_restores_gnu_deletion_offset(){check(Action::AwayBack,Some(3));}
    #[test] fn detached_and_reattached_marker_keeps_lifetime_identity(){check(Action::DetachReattach,Some(3));}
    #[test] fn detached_marker_stays_detached_on_undo(){check(Action::Detached,None);}
    #[test] fn cross_buffer_roundtrip_keeps_lifetime_identity(){check(Action::TransferBack,Some(3));}
    #[test] fn rebias_after_deletion_obeys_gnu_current_bias_offset(){check(Action::Rebias,Some(6));}
}
