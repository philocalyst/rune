//! Live sparse/vector keymap traversal. Every cursor is rooted before callbacks.
use super::*;
use crate::core::{gc::Rto,object::{Function,TRUE}};
use rune_core::macros::{root,call,list};
fn invoke(function:&Rto<Object>,key:&Rto<Object>,value:&Rto<Object>,env:&mut Rt<Env>,cx:&mut Context)->Result<()> {
    // Do not construct TypeError for arbitrary composites: its diagnostic
    // formatter can recursively traverse cyclic invalid callback graphs.
    let callable_tag=matches!(function.bind(cx).untag(),ObjectType::NIL|ObjectType::Symbol(_)|ObjectType::Cons(_)|ObjectType::SubrFn(_)|ObjectType::ByteFn(_));
    if !callable_tag {
        env.initialize_condition_metadata(cx);
        let data=list![function.bind(cx);cx];
        return Err(crate::eval::EvalError::signal(sym::INVALID_FUNCTION.into(),data,cx).into());
    }
    let callable:Function=function.bind(cx).try_into().expect("function tag preflight");
    root!(callable,cx);
    let value=value.bind(cx);
    let value=if value==TRUE {NIL}else{value};
    call!(callable,key.bind(cx),value;env,cx)?;
    Ok(())
}
fn walk<'a>(function:&Rto<Object>,keymap:&Rto<Object>,parents:bool,env:&mut Rt<Env>,cx:&'a mut Context)->Result<Object<'a>> {
    let map=match map_object(keymap.bind(cx),env,cx)? {
        Some(map)=>map,
        None=>{
            env.initialize_condition_metadata(cx);
            let data=rune_core::macros::list![sym::KEYMAPP,keymap.bind(cx);cx];
            return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(),data,cx).into());
        }
    };
    root!(map,cx);
    root!(cursor,map.bind(cx).cons().expect("checked map").cdr(),cx);
    // Saved outer LIST CELLS, not their cdrs: read the live cdr after nested
    // callbacks so parent attachment/detachment is observed like GNU keymap.c.
    root!(pending,Vec::<Object>::new(),cx);
    let mut steps=0usize;
    loop {
        steps+=1;ensure!(steps<=65536,"map-keymap traversal limit exceeded");
        env.poll_quit(cx)?;
        let current=cursor.bind(cx);
        let Some(cell)=current.cons() else {
            if parents {
                if let Some(parent)=map_object(current,env,cx)? {
                    cursor.set(parent.cons().expect("checked parent").cdr());
                    continue;
                }
                if let Some(saved)=pending.last() {
                    let saved=saved.bind(cx);
                    cursor.set(saved.cons().expect("rooted outer cell").cdr());pending.pop();continue;
                }
                return Ok(NIL);
            }
            return Ok(cursor.bind(cx));
        };
        if direct_map(current) {
            if !parents{return Ok(cursor.bind(cx));}
            cursor.set(cell.cdr());continue;
        }
        let binding=cell.car();
        if direct_map(binding) {
            if !parents{return Ok(cursor.bind(cx));}
            ensure!(pending.len()<256,"map-keymap embedded parent depth exceeded");
            pending.push(current);cursor.set(binding.cons().expect("checked embedded map").cdr());continue;
        }
        match binding.untag() {
            ObjectType::Cons(entry)=>{
                root!(key,entry.car(),cx);root!(value,entry.cdr(),cx);
                invoke(function,key,value,env,cx)?;
            },
            ObjectType::Vec(vector)=>{
                let len=vector.len();ensure!(len<=65536,"map-keymap vector limit exceeded");
                root!(vector,binding,cx);
                for index in 0..len {
                    steps+=1;ensure!(steps<=65536,"map-keymap traversal limit exceeded");
                    env.poll_quit(cx)?;
                    let ObjectType::Vec(current)=vector.bind(cx).untag() else{unreachable!()};
                    root!(value,current[index].get(),cx);
                    root!(key,Object::from(index as i64),cx);
                    invoke(function,key,value,env,cx)?;
                }
            }
            ObjectType::CharTable(_)=>bail!("map-keymap character-table traversal is unsupported"),
            _=>{}, // GNU ignores menu strings and malformed non-binding atoms.
        }
        // Rebind after callback/GC. Read THIS CELL's live cdr, never a stale
        // native iterator/borrow or an eagerly snapshotted future definition.
        let next=cursor.bind(cx).cons().expect("rooted current cell").cdr();
        cursor.set(next);
    }
}
pub(super) fn map_keymap(function:&Rto<Object>,keymap:&Rto<Object>,sort_first:crate::core::object::OptionalFlag,env:&mut Rt<Env>,cx:&mut Context)->Result<()> {
    if sort_first.is_some(){
        // Preserve the original Lisp sorting policy rather than inventing one.
        let sorted:Function=Object::from(sym::MAP_KEYMAP_SORTED).try_into()?;
        root!(sorted,cx);
        call!(sorted,function.bind(cx),keymap.bind(cx);env,cx)?;
    }else{walk(function,keymap,true,env,cx)?;}
    Ok(())
}
pub(super) fn map_keymap_internal<'a>(function:&Rto<Object>,keymap:&Rto<Object>,env:&mut Rt<Env>,cx:&'a mut Context)->Result<Object<'a>>{
    walk(function,keymap,false,env,cx)
}

#[cfg(test)]
mod tests {
    // Actual GNU macroexpand-all canonical forms, not production helper stubs.
    // The no-bootstrap unit fixture intentionally lacks subr.el's lambda/push.
    #[test]
    fn sparse_mapping_and_live_mutations_match_gnu_oracles() {
        for (name,source) in [
        ("sparse-order","(let ((m (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key m [98] 'b) (define-key m [f1] 'f) (define-key m [99] nil) (list (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) m) (nreverse out)))"),
        ("parent-shadow","(let ((m (make-sparse-keymap)) (p (make-sparse-keymap)) out) (define-key p [97] 'pa) (define-key p [98] 'pb) (define-key m [97] 'ma) (define-key m [99] nil) (set-keymap-parent m p) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) m) (nreverse out))"),
        ("prefix","(let ((m (make-sparse-keymap)) (p (make-sparse-keymap)) out) (define-key p [97] 'leaf) (define-key m [120] p) (map-keymap #'(lambda (k v) (setq out (cons (list k (if (keymapp v) 'prefix v)) out))) m) (nreverse out))"),
        ("default","(let ((m (make-sparse-keymap)) out) (define-key m [t] 'fallback) (define-key m [97] 'a) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) m) (nreverse out))"),
        ("symbol-map","(unwind-protect (progn (fset 'rune-map-oracle (let ((m (make-sparse-keymap))) (define-key m [97] 'a) m)) (let (out) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) 'rune-map-oracle) (nreverse out))) (fmakunbound 'rune-map-oracle))"),
        ("non-map","(condition-case e (map-keymap #'ignore '(x (97 . a))) (error (car e)))"),
        ("malformed-entry","(let (out) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) '(keymap junk (97 . a) (98 b c))) (nreverse out))"),
        ("improper-tail","(let (out) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) '(keymap (97 . a) . broken)) (nreverse out))"),
        ("callback-error","(condition-case e (let ((n 0) (m (make-sparse-keymap))) (define-key m [97] 'a) (define-key m [98] 'b) (list (condition-case e (map-keymap #'(lambda (k v) (setq n (1+ n)) (signal 'error '(\"callback\"))) m) (error e)) n)) (error (car e)))"),
        ("mutate-add","(let ((m (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key m [98] 'b) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out)) (define-key m [99] 'c)) m) (nreverse out))"),
        ("mutate-next","(let ((m (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key m [98] 'b) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out)) (define-key m [97] 'changed)) m) (nreverse out))"),
        ("mutate-parent","(let ((m (make-sparse-keymap)) (p (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key p [98] 'b) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out)) (set-keymap-parent m p)) m) (nreverse out))"),
        ("raw-cycle","(let ((m (list 'keymap (cons 97 'a))) (n 0)) (setcdr (cdr m) (cdr m)) (catch 'stop (map-keymap #'(lambda (k v) (setq n (1+ n)) (if (> n 10) (throw 'stop (list 'repeated n)))) m)))"),
        ("future-delete","(let ((m (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key m [98] 'b) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out)) (define-key m [97] nil)) m) (nreverse out))"),
        ("parent-rebind","(let ((m (make-sparse-keymap)) (p (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key p [98] 'b) (set-keymap-parent m p) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out)) (define-key p [98] 'changed)) m) (nreverse out))"),
        ("parent-detach","(let ((m (make-sparse-keymap)) (p (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key p [98] 'b) (set-keymap-parent m p) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out)) (set-keymap-parent m nil)) m) (nreverse out))"),
        ("internal-parent","(let ((m (make-sparse-keymap)) (p (make-sparse-keymap)) out) (define-key m [97] 'a) (define-key p [98] 'b) (set-keymap-parent m p) (list (eq (map-keymap-internal #'(lambda (k v) (setq out (cons (list k v) out))) m) p) (nreverse out)))"),
        ] { eprintln!("map-keymap oracle {name}"); crate::assert_elprop!("{}",source); }
    }
    #[test]
    fn cyclic_composite_callback_signals_without_rendering_graph() {
        use super::*;
        use crate::core::{gc::RootSet,object::IntoObject};
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        sym::init_symbols();
        let constants=vec![NIL].into_obj(&cx.block);root!(constants,cx);
        let codes=vec![135_u8].into_obj(&cx.block);
        let function=crate::alloc::make_byte_code(0,codes.untag(),constants.bind(cx).untag(),1,None,None,&[],cx).unwrap();root!(function,cx);
        constants.bind(cx).untag().try_mut().unwrap()[0].set(function.bind(cx).into());
        let callback=cx.add(vec![Object::from(function.bind(cx))]);root!(callback,cx);
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        let name=crate::lread::intern("map-invalid-cycle-callback",cx);env.vars.insert(name,callback.bind(cx));
        let form=crate::reader::read("(condition-case e (map-keymap map-invalid-cycle-callback '(keymap (97 . a))) (invalid-function (eq (car (cdr e)) map-invalid-cycle-callback)))",cx).unwrap().0;root!(form,cx);
        let result=crate::interpreter::eval(form,None,env,cx).unwrap();
        assert_eq!(result,TRUE);
        cx.garbage_collect(true);
        let form=crate::reader::read("(condition-case e (map-keymap map-invalid-cycle-callback '(keymap (97 . a))) (invalid-function (eq (car (cdr e)) map-invalid-cycle-callback)))",cx).unwrap().0;root!(form,cx);
        assert_eq!(crate::interpreter::eval(form,None,env,cx).unwrap(),TRUE);
    }
    #[test]
    fn callback_gc_recursion_and_partial_error_keep_live_cursors_rooted() {
        crate::assert_elprop!("{}","(let ((m (make-sparse-keymap)) out) (define-key m [97] (list 'a)) (define-key m [98] (list 'b)) (map-keymap #'(lambda (k v) (garbage-collect) (setq out (cons (list k (car v)) out)) (if (= k 98) (progn (let ((inner (make-sparse-keymap))) (define-key inner [99] 'c) (map-keymap #'(lambda (ik iv) (garbage-collect) (setq out (cons (list ik iv) out))) inner))))) m) (nreverse out))");
        crate::assert_elprop!("{}","(map-keymap 42 (make-sparse-keymap))");
        crate::assert_elprop!("(condition-case e (map-keymap 42 '(keymap (97 . a))) (error e))");
        crate::assert_elprop!("{}","(let ((m (make-sparse-keymap))) (define-key m [97] t) (let (out) (map-keymap #'(lambda (k v) (setq out (cons (list k v) out))) m) out))");
    }
}
