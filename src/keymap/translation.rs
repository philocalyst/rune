//! Scoped owner/logical-terminal function-key fallback; no UI Lisp or callbacks.
use super::*;
pub(super) fn initial_local_map<'a>(env:&Rt<Env>,cx:&'a Context)->Object<'a>{
    let parent=env.variable_value(sym::FUNCTION_KEY_MAP,cx).expect("fresh function map variable").expect("global map initializes before local map");
    let map=make_sparse_keymap(None,cx);
    set_keymap_parent(map,parent,env,cx).expect("fresh local function-map parent");
    map
}
/// Only genuine one-event sequence outputs are supported in this native slice.
/// Inspect at most two scalar/byte elements, never flatten an arbitrary string.
fn one_output(value:Object)->Result<Object>{
    let event=match value.untag(){
        ObjectType::Vec(v)=>{ensure!(v.len()==1,"function-key translation requires one output event");v[0].get()},
        ObjectType::String(s)=>{let mut chars=s.chars();let c=chars.next().ok_or_else(||anyhow!("empty function-key translation unsupported"))?;ensure!(chars.next().is_none(),"multi-event function-key translation unsupported");Object::from(c as i64)},
        ObjectType::ByteString(s)=>{ensure!(s.len()==1,"function-key translation requires one output byte");Object::from(i64::from(s[0]))},
        _=>bail!("callable or prefix function-key translation unsupported"),
    };
    ensure!(matches!(event.untag(),ObjectType::Int(n) if n>=0 && n<=i64::from(u32::MAX)) || matches!(event.untag(),ObjectType::Symbol(_)),"invalid function-key translation event");
    Ok(event)
}
pub(super) fn fallback_definition<'a>(active:Object<'a>,key:Object<'a>,env:&Rt<Env>,cx:&'a Context)->Result<Option<Object<'a>>>{
    let local=env.variable_value(sym::LOCAL_FUNCTION_KEY_MAP,cx)?.unwrap_or(NIL);
    if local.is_nil(){return Ok(None);}
    let translated=lookup_key(local,key,None,env,cx)?;
    if translated.is_nil(){return Ok(None);}
    let output=one_output(translated)?;
    let output=cx.add(vec![output]);
    // Deliberately no recursive function-map lookup of translated output.
    let definition=lookup_key(active,output,None,env,cx)?;
    ensure!(!definition.is_nil(),"translated function-key event has no active binding");
    Ok(Some(definition))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_core::macros::root;
    #[test]
    fn c_initialization_has_distinct_rooted_maps_and_captured_parent(){
        let roots=crate::core::gc::RootSet::default();let cx=&mut Context::new(&roots);sym::init_symbols();
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
        let global=env.variable_value(sym::FUNCTION_KEY_MAP,cx).unwrap().unwrap();let local=env.variable_value(sym::LOCAL_FUNCTION_KEY_MAP,cx).unwrap().unwrap();
        assert!(!global.ptr_eq(local));assert!(keymapp(global,env,cx).unwrap());assert!(keymapp(local,env,cx).unwrap());assert!(keymap_parent(local,env,cx).unwrap().ptr_eq(global));
        let old=global;root!(old,cx);let local=local;root!(local,cx);
        let replacement=make_sparse_keymap(None,cx);env.set_var_context(sym::FUNCTION_KEY_MAP,replacement,cx).unwrap();
        cx.garbage_collect(true);assert!(keymap_parent(local.bind(cx),env,cx).unwrap().ptr_eq(old.bind(cx)));
    }
    #[test]
    fn real_owner_dispatch_consumes_parent_local_priority_and_nonrecursive_outputs(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(setq function-map-hit 0) (setq function-active-map (make-sparse-keymap)) (define-key function-active-map [f7] '(lambda () (interactive) (setq function-map-hit 1))) (define-key function-active-map [f8] '(lambda () (interactive) (setq function-map-hit 2))) (use-global-map function-active-map) (define-key function-key-map [f6] [f7]) (define-key function-key-map [f7] [f8])").map_err(anyhow::Error::msg)?;
            use rune_editor::EditorHost;let snapshot=host.snapshot()?;let view=snapshot.views.iter().find(|v|v.id==snapshot.selected_window).unwrap();let(w,b)=(view.id.as_u64(),view.view.buffer.as_u64());drop(snapshot);
            host.dispatch_native_key(w,b,"f6",false,false,false).map_err(anyhow::Error::msg)?;
            host.evaluate_source("(if (= function-map-hit 1) nil (signal 'error '(\"output must not retranslate\"))) (define-key local-function-key-map [f6] [f8])").map_err(anyhow::Error::msg)?;
            host.dispatch_native_key(w,b,"f6",false,false,false).map_err(anyhow::Error::msg)?;
            host.evaluate_source("(if (= function-map-hit 2) nil (signal 'error '(\"local override failed\"))) (define-key function-active-map [f6] '(lambda () (interactive) (setq function-map-hit 3)))").map_err(anyhow::Error::msg)?;
            host.dispatch_native_key(w,b,"f6",false,false,false).map_err(anyhow::Error::msg)?;
            host.evaluate_source("(if (= function-map-hit 3) nil (signal 'error '(\"ordinary binding must win\"))) (garbage-collect)").map_err(anyhow::Error::msg)?;
            host.dispatch_native_key(w,b,"f6",false,false,false).map_err(anyhow::Error::msg)?;
            Ok(())
        }).unwrap();
    }
    #[test]
    fn translated_prefix_survives_gc_and_unsupported_output_never_falls_back_to_original_text(){
        with_lisp_runtime_owner(80,24,|mut host|{
            use rune_editor::{EditorHost,Document};
            host.evaluate_source("(setq function-map-hit 0 function-active-map (make-sparse-keymap)) (define-key function-active-map [f7 120] '(lambda () (interactive) (setq function-map-hit 4))) (define-key function-active-map [120] '(lambda () (interactive) (setq function-map-hit 5))) (use-global-map function-active-map) (define-key function-key-map [f6] [f7])").map_err(anyhow::Error::msg)?;
            let snapshot=host.snapshot()?;let view=snapshot.views.iter().find(|v|v.id==snapshot.selected_window).unwrap();let(w,b)=(view.id.as_u64(),view.view.buffer.as_u64());drop(snapshot);
            assert!(host.dispatch_native_key(w,b,"f6",false,false,false).map_err(anyhow::Error::msg)?);
            host.evaluate_source("(garbage-collect)").map_err(anyhow::Error::msg)?;
            host.dispatch_native_key(w,b,"x",false,false,false).map_err(anyhow::Error::msg)?;
            host.evaluate_source("(if (= function-map-hit 4) nil (signal 'error '(\"prefix lost\"))) (define-key local-function-key-map [f6] [f7 f8])").map_err(anyhow::Error::msg)?;
            assert!(host.dispatch_native_key(w,b,"f6",false,false,false).is_err());
            host.dispatch_native_key(w,b,"x",false,false,false).map_err(anyhow::Error::msg)?;
            host.evaluate_source("(if (= function-map-hit 5) nil (signal 'error '(\"unsupported output retained stale prefix\"))) (define-key local-function-key-map [f6] [not-bound])").map_err(anyhow::Error::msg)?;
            assert!(host.dispatch_native_key(w,b,"f6",false,false,false).is_err());
            assert!(host.snapshot()?.buffers.iter().all(|b|b.document.to_text().is_empty()));
            Ok(())
        }).unwrap();
    }
    #[test]
    fn translation_output_preflight_is_bounded_and_no_graph_formatter_runs(){
        let roots=crate::core::gc::RootSet::default();let cx=&Context::new(&roots);
        assert!(one_output(cx.add("a".repeat(1_000_000))).is_err());
        assert!(one_output(cx.add(vec![Object::from(1),Object::from(2)])).is_err());
        assert!(one_output(cx.add(vec![cx.add(vec![NIL])])).is_err());
        assert_eq!(one_output(cx.add("λ")).unwrap(),i64::from('λ' as u32));
    }
}
