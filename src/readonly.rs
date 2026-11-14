//! Read-only preflight for original Lisp editing/shell helpers, not mutation bypass.
use crate::core::{env::{Env,sym}, gc::{Context,Rt}, object::{LispBuffer,NIL,Object,ObjectType}};
use anyhow::{Result,bail};
use rune_macros::defun;
use rune_core::macros::list;
defvar!(INHIBIT_READ_ONLY);
fn signal(condition: crate::core::object::Symbol, data: Object, env: &mut Rt<Env>, cx: &Context) -> anyhow::Error {
    env.initialize_condition_metadata(cx);
    crate::eval::EvalError::signal(condition.into(),data,cx).into()
}
#[defun]
fn barf_if_buffer_read_only(position: Option<Object>, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    // GNU validates an explicit position's TYPE even if the read-only check will
    // be bypassed. Do not render invalid objects (which may contain cycles).
    let explicit: Option<i64> = match position.filter(|v| !v.is_nil()) {
        None => None,
        Some(value) if matches!(value.untag(),ObjectType::Int(_)) => Some(value.try_into()?),
        Some(value) => return Err(signal(sym::WRONG_TYPE_ARGUMENT,list!(sym::FIXNUMP,value;cx),env,cx)),
    };
    let handle=env.current_buffer_handle();
    let gateway=env.editor_gateway();
    if gateway.read_only_value(handle,cx)?.is_nil() || env.variable_value(sym::INHIBIT_READ_ONLY,cx)?.is_some_and(|v|!v.is_nil()) { return Ok(()); }
    let read=env.buffer_read(cx)?;
    let point_max=read.document.text().len_chars()+1;
    let position=explicit.unwrap_or((read.point+1) as i64);
    if position<1 || position as u128>point_max as u128 {
        return Err(signal(sym::ARGS_OUT_OF_RANGE,list!(position,position;cx),env,cx));
    }
    // Endpoint has no character; existing property lookup returns nil there.
    let plist=crate::textprops::text_properties_at(position as usize,None,env,cx)?;
    let inhibit=character_inhibit(plist)?;
    if !inhibit.is_nil() { return Ok(()); }
    let buffer=cx.add(LispBuffer::for_handle(handle,cx)?);
    Err(signal(sym::BUFFER_READ_ONLY,list!(buffer;cx),env,cx))
}
// The public buffer plist can be mutated by Lisp; never traverse an unbounded
// spine or format its arbitrary values in a rejection message.
fn character_inhibit(mut plist: Object) -> Result<Object> {
    for _ in 0..4096 {
        if plist.is_nil() { return Ok(NIL); }
        let ObjectType::Cons(key)=plist.untag() else { bail!("Malformed inhibit-read-only property plist"); };
        let ObjectType::Cons(value)=key.cdr().untag() else { bail!("Odd inhibit-read-only property plist"); };
        if crate::fns::eq(key.car(),sym::INHIBIT_READ_ONLY.into()) { return Ok(value.car()); }
        plist=value.cdr();
    }
    if plist.is_nil() { return Ok(NIL); }
    bail!("inhibit-read-only property plist exceeds traversal limit")
}
#[cfg(test)]
mod tests {
    #[test]
    fn property_pair_budget_accepts_exact_limit_and_rejects_more() {
        use crate::core::{cons::Cons,env::sym,gc::{Context,RootSet},object::{NIL,TRUE,Object}};
        let roots=RootSet::default();let cx=Context::new(&roots);
        let pair=|key:Object,value:Object,tail:Object| -> Object { Cons::new(key,Cons::new(value,tail,&cx),&cx).into() };
        let mut plist=NIL;
        for _ in 0..4095 { plist=pair(NIL,NIL,plist); }
        assert!(super::character_inhibit(plist).unwrap().is_nil());
        plist=pair(NIL,NIL,plist);
        assert!(super::character_inhibit(plist).unwrap().is_nil());
        plist=pair(NIL,NIL,plist);
        assert!(super::character_inhibit(plist).is_err());
        let mut last=pair(sym::INHIBIT_READ_ONLY.into(),TRUE,NIL);
        for _ in 0..4095 { last=pair(NIL,NIL,last); }
        assert!(super::character_inhibit(last).unwrap().ptr_eq(TRUE));
    }

    #[test]
    fn cyclic_property_plist_is_bounded_without_rendering() {
        use crate::core::{cons::Cons,gc::{Context,RootSet},object::NIL};
        let roots=RootSet::default();let cx=Context::new(&roots);
        let value=Cons::new1(NIL,&cx);let key=Cons::new(NIL,value,&cx);
        value.set_cdr(key.into()).unwrap();
        assert!(super::character_inhibit(key.into()).is_err());
    }

    #[test]
    fn read_only_preflight_matches_gnu_owner_state_and_conditions() {
        for flag in ["nil","t","'readonly-token"] {
            for inhibit in ["nil","t","'(ignored)"] {
                for position in ["nil","0","1","2","3","4","-1"] {
                    crate::assert_elprop!("(progn (erase-buffer) (insert \"ab\") (goto-char 2) (setq buffer-read-only {}) (let ((inhibit-read-only {})) (list (condition-case e (barf-if-buffer-read-only {}) (error (list (car e) (if (eq (car e) 'buffer-read-only) (bufferp (car (cdr e))) (cdr e))))) buffer-read-only (point))))",flag,inhibit,position);
                }
            }
        }
    }
    #[test]
    fn read_only_preflight_honors_character_override_and_safe_type_error() {
        crate::assert_elprop!(r#"(progn (erase-buffer) (insert (propertize "ab" 'inhibit-read-only t)) (goto-char 1) (setq buffer-read-only t) (let ((inhibit-read-only nil)) (list (barf-if-buffer-read-only) (barf-if-buffer-read-only 2) (condition-case e (barf-if-buffer-read-only 3) (error (car e))) buffer-read-only (point))))"#);
        crate::assert_elprop!(r#"(let ((v (vector nil)) (inhibit-read-only t)) (aset v 0 v) (condition-case e (barf-if-buffer-read-only v) (wrong-type-argument (list (car e) (car (cdr e)) (eq v (car (cdr (cdr e))))))))"#);
    }
}
