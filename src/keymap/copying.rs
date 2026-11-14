//! Owner-local keymap copying. No global transfer, callbacks, or recursive walk.
use super::*;
use rune_core::macros::list;
#[derive(Clone,Copy)]
enum Mode { Map, Spine, Entry, Definition, Leaf, Menu(usize) }
enum Task<'a> {
    Visit(Object<'a>,Mode,usize),
    Cons,
    Vector(usize),
    Leave((usize,u8)),
}
pub(super) fn copy<'a>(input:Object<'a>,env:&Rt<Env>,cx:&'a Context)->Result<Object<'a>> {
    let Some(map)=map_object(input,env,cx)? else {
        let data=rune_core::macros::list![sym::KEYMAPP,input;cx];
        return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(),data,cx).into());
    };
    let mut tasks=vec![Task::Visit(map,Mode::Map,0)];
    let mut values=Vec::new();
    let mut active=std::collections::HashSet::new();
    let mut work=0usize;
    while let Some(task)=tasks.pop() {
        work+=1;ensure!(work<=100_000,"copy-keymap work budget exceeded");
        match task {
            Task::Leave(id)=>{active.remove(&id);},
            Task::Cons=>{
                let cdr=values.pop().expect("copy cdr task");
                let car=values.pop().expect("copy car task");
                values.push(Cons::new(car,cdr,cx).into());
            },
            Task::Vector(len)=>{
                let offset=values.len().checked_sub(len).expect("copy vector tasks");
                let vector=values.split_off(offset);values.try_reserve(1)?;values.push(cx.add(vector));
            },
            Task::Visit(value,mode,depth)=>{
                ensure!(depth<=128,"copy-keymap depth budget exceeded");
                match mode {
                    Mode::Map=>{
                        let pair=value.cons().expect("resolved direct map");
                        let id=(value.into_raw().addr(),0);
                        active.try_reserve(1)?;ensure!(active.insert(id),"Cyclic prefix keymap is unsupported");
                        tasks.try_reserve(4)?;
                        tasks.extend([Task::Leave(id),Task::Cons,Task::Visit(pair.cdr(),Mode::Spine,depth+1),Task::Visit(pair.car(),Mode::Leaf,depth+1)]);
                    },
                    Mode::Spine if value.is_nil() || direct_map(value)=>{
                        // A keymap in the tail is the shared parent, not a local
                        // prefix definition to copy recursively.
                        values.try_reserve(1)?;values.push(value);
                    },
                    Mode::Spine=>{
                        let Some(pair)=value.cons() else {bail!("Malformed keymap spine is unsupported")};
                        let id=(value.into_raw().addr(),1);
                        active.try_reserve(1)?;ensure!(active.insert(id),"Cyclic keymap spine is unsupported");
                        tasks.try_reserve(4)?;
                        tasks.extend([Task::Leave(id),Task::Cons,Task::Visit(pair.cdr(),Mode::Spine,depth),Task::Visit(pair.car(),Mode::Entry,depth)]);
                    },
                    Mode::Entry=>match value.untag() {
                        ObjectType::Cons(pair)=>{
                            tasks.try_reserve(3)?;
                            tasks.extend([Task::Cons,Task::Visit(pair.cdr(),Mode::Definition,depth),Task::Visit(pair.car(),Mode::Leaf,depth)]);
                        },
                        ObjectType::Vec(vector)=>{
                            ensure!(vector.len()<=100_000-work,"copy-keymap vector budget exceeded");
                            tasks.try_reserve(vector.len()+1)?;
                            tasks.push(Task::Vector(vector.len()));
                            for value in vector.iter().rev() {tasks.push(Task::Visit(value.get(),Mode::Definition,depth));}
                        },
                        ObjectType::CharTable(_)=>bail!("copy-keymap character-table storage is unsupported"),
                        _=>{values.try_reserve(1)?;values.push(value);},
                    },
                    Mode::Menu(index)=>{
                        if value.is_nil() {values.try_reserve(1)?;values.push(value);continue;}
                        let Some(pair)=value.cons() else {bail!("Malformed menu-item list is unsupported")};
                        let id=(value.into_raw().addr(),2);
                        active.try_reserve(1)?;ensure!(active.insert(id),"Cyclic menu-item list is unsupported");
                        tasks.try_reserve(4)?;
                        let car_mode=if index==2 {Mode::Definition}else{Mode::Leaf};
                        tasks.extend([Task::Leave(id),Task::Cons,Task::Visit(pair.cdr(),Mode::Menu(index+1),depth),Task::Visit(pair.car(),car_mode,depth)]);
                    },
                    Mode::Definition if direct_map(value)=>{
                        tasks.try_reserve(1)?;tasks.push(Task::Visit(value,Mode::Map,depth+1));
                    },
                    Mode::Definition if value.cons().is_some_and(|pair|pair.car().ptr_eq(Object::from(sym::MENU_ITEM)))=>{
                        tasks.try_reserve(1)?;tasks.push(Task::Visit(value,Mode::Menu(0),depth));
                    },
                    Mode::Definition if value.cons().is_some_and(|pair|matches!(pair.car().untag(),ObjectType::String(_)|ObjectType::ByteString(_)))=>{
                        let pair=value.cons().expect("old menu tag preflight");
                        let id=(value.into_raw().addr(),3);
                        active.try_reserve(1)?;ensure!(active.insert(id),"Cyclic old menu definition is unsupported");
                        tasks.try_reserve(4)?;
                        tasks.extend([Task::Leave(id),Task::Cons,Task::Visit(pair.cdr(),Mode::Definition,depth+1),Task::Visit(pair.car(),Mode::Leaf,depth)]);
                    },
                    Mode::Definition | Mode::Leaf=>{values.try_reserve(1)?;values.push(value);},
                }
            },
        }
        ensure!(tasks.len()<=100_000 && values.len()<=100_000,"copy-keymap task budget exceeded");
    }
    ensure!(values.len()==1,"Invalid copy-keymap result stack");
    Ok(values.pop().expect("checked result"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn sparse_copy_mutation_prefix_parent_and_leaf_identity_match_gnu() {
        for source in [
            "(let* ((m (make-sparse-keymap)) (leaf (list 'command))) (define-key m [97] leaf) (let ((c (copy-keymap m))) (define-key c [98] 'new) (list (eq m c) (eq (lookup-key c [97]) leaf) (lookup-key m [98]) (lookup-key c [98]))))",
            "(let ((m (make-sparse-keymap)) (p (make-sparse-keymap))) (define-key p [97] 'parent) (set-keymap-parent m p) (let ((c (copy-keymap m))) (list (eq (keymap-parent c) p) (lookup-key c [97]))))",
            "(let ((m (make-sparse-keymap)) (p (make-sparse-keymap))) (define-key p [97] 'old) (define-key m [120] p) (let ((c (copy-keymap m))) (define-key (lookup-key c [120]) [97] 'new) (list (eq (lookup-key c [120]) p) (lookup-key m [120 97]) (lookup-key c [120 97]))))",
            "(let* ((m (make-sparse-keymap)) (p (make-sparse-keymap))) (define-key m [97] p) (define-key m [98] p) (let ((c (copy-keymap m))) (list (eq (lookup-key c [97]) (lookup-key c [98])) (eq p (lookup-key c [97])))))",
            "(let* ((m (make-sparse-keymap)) (v (list 'menu-item \"Label\" 'ignore :help \"Help\"))) (define-key m [97] v) (let* ((c (copy-keymap m)) (a (cdr (assq 97 (cdr m)))) (b (cdr (assq 97 (cdr c))))) (list (eq a b) (eq (cdr a) (cdr b)) (eq (car (cdr a)) (car (cdr b))))))",
            "(let* ((m (make-sparse-keymap)) (p (make-sparse-keymap))) (define-key p [98] 'ignore) (define-key m [97] (list 'menu-item \"Submenu\" p)) (let* ((c (copy-keymap m)) (b (cdr (assq 97 (cdr c))))) (list (eq p (car (cdr (cdr b)))) (lookup-key (car (cdr (cdr b))) [98]))))",
            "(let* ((m (make-sparse-keymap)) (v (list \"Label\" 'ignore))) (define-key m [97] v) (let* ((c (copy-keymap m)) (b (cdr (assq 97 (cdr c))))) (list (eq v b) (eq (cdr v) (cdr b)) (eq (car v) (car b)))))",
            "(let* ((m (make-sparse-keymap)) (p (make-sparse-keymap)) (v (cons \"Label\" p))) (define-key m [97] v) (let* ((c (copy-keymap m)) (b (cdr (assq 97 (cdr c))))) (list (eq v b) (eq p (cdr b)))))",
            "(condition-case e (copy-keymap 42) (wrong-type-argument (list (car e) (car (cdr e)) (car (cdr (cdr e))))))",
        ] {crate::assert_elprop!("{}",source);}
    }
    #[test]
    fn owner_copy_keeps_uninterned_leaves_after_gc_and_rejects_cycles_statically() {
        crate::interpreter::assert_lisp("(let* ((m (make-sparse-keymap)) (v (make-symbol \"copy-leaf\"))) (define-key m [97] v) (let ((c (copy-keymap m))) (setq m nil) (garbage-collect) (eq (lookup-key c [97]) v)))", "t");
        crate::interpreter::assert_lisp("(let ((m (make-sparse-keymap))) (define-key m [97] m) (condition-case nil (copy-keymap m) (error t)))", "t");
        crate::interpreter::assert_lisp("(let ((v (vector nil))) (aset v 0 v) (condition-case e (copy-keymap v) (wrong-type-argument (eq (car (cdr (cdr e))) v))))", "t");
    }

}
