//! Constructor-only opaque foundation. Not emitted by the evaluator and not
//! admitted into Function conversion: call/producer/ABI migration is separate.
use super::{Gc, IntoObject, LispVec, Object, ObjectType, ObjCell};
use crate::core::gc::{Context, GcHeap};
use crate::derive_GcMoveable;
use anyhow::{Result, ensure};
use rune_macros::Trace;
use std::fmt;

#[derive(Trace)]
// Heap-internal edge uses ObjCell, NOT root Slot: ObjCell moves and queues
// children without recursively draining the collector's trace stack.
struct InterpretedFields { fields: ObjCell }
#[derive(Trace)]
pub(crate) struct InterpretedFn(GcHeap<InterpretedFields>);
derive_GcMoveable!(InterpretedFn);
impl InterpretedFn {
    /// SAFETY: all field graphs belong to cx. Caller must root the returned
    /// value before collection. Shared cx excludes collection during allocation.
    pub(crate) unsafe fn new<'a>(args: Object<'a>, body: Object<'a>, env: Object<'a>,
        doc: Object<'a>, interactive: Option<Object<'a>>, cx: &'a Context) -> Result<Gc<&'a Self>> {
        ensure!(matches!(body.untag(), ObjectType::Cons(_)), "Interpreted body must be a Cons");
        ensure!(args.is_nil() || matches!(args.untag(), ObjectType::Cons(_)), "Interpreted arguments must be a list");
        let mut fields = vec![args, body, env];
        if !doc.is_nil() || interactive.is_some() { fields.extend([super::NIL, doc]); }
        if let Some(spec) = interactive { fields.push(spec); }
        let fields: Gc<&LispVec> = cx.add_as(fields.as_slice());
        Ok(Self(GcHeap::new(InterpretedFields {
            fields: unsafe { ObjCell::new(Object::from(fields)) },
        }, false)).into_obj(&cx.block))
    }
    fn fields(&self) -> &LispVec {
        // Private cell is initialized exactly once to a fresh owner vector.
        // Moving GC preserves its tag; no mutable cell/vector view escapes.
        match self.0.fields.get().untag() {
            ObjectType::Vec(fields) => fields,
            _ => unreachable!("invalid private interpreted field vector"),
        }
    }
    pub(crate) fn len(&self) -> usize { self.fields().len() }
    pub(crate) fn field(&self, index: usize) -> Option<Object<'_>> { self.fields().get(index).map(|x| x.get()) }
    pub(crate) fn args(&self) -> Object<'_> { self.field(0).unwrap() }
    pub(crate) fn body(&self) -> Object<'_> { self.field(1).unwrap() }
    pub(crate) fn environment(&self) -> Object<'_> { self.field(2).unwrap() }
    pub(crate) fn docstring(&self) -> Object<'_> { self.field(4).unwrap_or(super::NIL) }
    pub(crate) fn interactive(&self) -> Option<Object<'_>> { self.field(5) }
    pub(crate) fn fields_snapshot(&self, limit: usize) -> Result<Vec<Object<'_>>> {
        ensure!(self.len() <= limit, "Interpreted field snapshot limit exceeded");
        Ok((0..self.len()).map(|i| self.field(i).unwrap()).collect())
    }
}
// Rust equality is identity. Lisp contextual equality handles bounded fields;
// structural hash keys remain rejected instead of violating Hash/Eq contracts.
impl PartialEq for InterpretedFn { fn eq(&self, other: &Self) -> bool { std::ptr::eq(self, other) } }
impl Eq for InterpretedFn {}
impl fmt::Display for InterpretedFn { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("#<unsupported-interpreted-function>") } }
impl fmt::Debug for InterpretedFn { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(self, f) } }

#[cfg(test)]
mod tests {
 use super::*;
 use super::super::{CloneIn, Function, NIL};
 use crate::{root, core::{cons::Cons, gc::RootSet}};
 #[test]
 fn interpreted_layout_gc_and_boundaries() {
  let roots=RootSet::default(); let cx=&mut Context::new(&roots);
  let body=Object::from(Cons::new(cx.add("retained-body"),NIL,&cx.block));
  // SAFETY: immediate/current-owner fields; immediately rooted before GC.
  let fun=unsafe { InterpretedFn::new(NIL,body,NIL,NIL,Some(NIL),cx).unwrap() };
  root!(object,Object::from(fun),cx);
  assert_eq!(fun.untag().len(),6); assert!(fun.untag().interactive().unwrap().is_nil());
  assert!(fun.untag().fields_snapshot(5).is_err());
  assert!(Function::try_from(Object::from(fun)).is_err());
  assert!(Object::from(fun).clone_in(&cx.block).is_err());
  assert!(crate::property_guard::reject_property_strings(Object::from(fun)).is_err());
  for _ in 0..3 { cx.garbage_collect(true);
   let ObjectType::InterpretedFn(f)=object.bind(cx).untag() else {panic!("lost type")};
   assert_eq!(f.len(),6); assert!(f.args().is_nil()); assert!(f.environment().is_nil()); assert!(f.docstring().is_nil());
   let ObjectType::Cons(body)=f.body().untag() else {panic!("lost body")};
   assert_eq!(body.car().to_string(),"\"retained-body\"");
  }
 }
 #[test]
 fn interpreted_readonly_fields_and_equal() {
  let roots=RootSet::default(); let cx=&Context::new(&roots);
  let body=Object::from(Cons::new(7,NIL,&cx.block));
  // SAFETY: current-owner graph; shared Context excludes GC and callbacks.
  unsafe {
   let a=Object::from(InterpretedFn::new(NIL,body,NIL,NIL,None,cx).unwrap());
   let b=Object::from(InterpretedFn::new(NIL,body,NIL,NIL,None,cx).unwrap());
   let cmd=Object::from(InterpretedFn::new(NIL,body,NIL,NIL,Some(NIL),cx).unwrap());
   let doc=Object::from(InterpretedFn::new(NIL,body,NIL,cx.add("Doc"),None,cx).unwrap());
   assert_eq!(crate::fns::length(a).unwrap(),3); assert_eq!(crate::fns::length(doc).unwrap(),5); assert_eq!(crate::fns::length(cmd).unwrap(),6);
   assert!(crate::data::aref(a,1,cx).unwrap().ptr_eq(body)); assert!(crate::data::aref(a,3,cx).is_err());
   assert!(crate::contextual_equal::equal(a,b,cx).unwrap()); assert!(!a.ptr_eq(b));
   assert!(!crate::contextual_equal::equal(a,cmd,cx).unwrap()); assert!(!crate::contextual_equal::equal(a,doc,cx).unwrap());
   assert!(crate::contextual_equal::reject_marker_hash_key(a).is_err());
   assert!(InterpretedFn::new(NIL,NIL,NIL,NIL,None,cx).is_err());
   assert!(InterpretedFn::new(7.into(),body,NIL,NIL,None,cx).is_err());
  }
 }
 #[test]
 fn interpreted_transitive_cycles_gc_and_bounded_equal() {
  let roots=RootSet::default(); let cx=&mut Context::new(&roots);
  let body=Object::from(Cons::new(NIL,NIL,&cx.block));
  let ea=Cons::new(1,NIL,&cx.block); let eb=Cons::new(1,NIL,&cx.block);
  // SAFETY: sameowner nodes, no GC before roots; cycles use traced Cons edges.
  let(a,b)=unsafe {(Object::from(InterpretedFn::new(NIL,body,ea.into(),NIL,None,cx).unwrap()),Object::from(InterpretedFn::new(NIL,body,eb.into(),NIL,None,cx).unwrap()))};
  ea.set_cdr(a).unwrap(); eb.set_cdr(b).unwrap(); root!(a,a,cx); root!(b,b,cx);
  for _ in 0..3 {cx.garbage_collect(true); let av=a.bind(cx); let bv=b.bind(cx);
   assert!(crate::contextual_equal::equal(av,av,cx).unwrap()); assert!(crate::contextual_equal::equal(av,bv,cx).is_err());
   let ObjectType::InterpretedFn(f)=av.untag() else {panic!("lost function")};
   let ObjectType::Cons(e)=f.environment().untag() else {panic!("lost environment")}; assert!(e.cdr().ptr_eq(av));
  }
 }
 #[test]
 fn interpreted_deep_heap_edges_queue_without_recursive_root_slot_trace() {
  let roots=RootSet::default(); let cx=&mut Context::new(&roots);
  let body=Object::from(Cons::new(NIL,NIL,&cx.block));
  let mut chain=NIL;
  for _ in 0..20_000 {
   // SAFETY: shared cx excludes GC; previous chain and body are sameowner.
   chain=Object::from(unsafe {InterpretedFn::new(NIL,body,chain,NIL,None,cx).unwrap()});
  }
  root!(chain,chain,cx);
  for _ in 0..3 { cx.garbage_collect(true);
   let mut node=chain.bind(cx); let mut count=0;
   while let ObjectType::InterpretedFn(fun)=node.untag() {
    node=fun.environment(); count+=1;
   }
   assert!(node.is_nil()); assert_eq!(count,20_000);
  }
 }
 #[test]
 fn interpreted_deep_contextual_equal_uses_bounded_native_worklist() {
  let roots=RootSet::default(); let cx=&Context::new(&roots);
  let body=Object::from(Cons::new(NIL,NIL,&cx.block));
  let(mut a,mut b)=(NIL,NIL);
  for _ in 0..600 {
   // SAFETY: all graph fields in this owner, no GC/callback under shared cx.
   unsafe {
    a=Object::from(InterpretedFn::new(NIL,body,a,NIL,None,cx).unwrap());
    b=Object::from(InterpretedFn::new(NIL,body,b,NIL,None,cx).unwrap());
   }
  }
  assert!(crate::contextual_equal::equal(a,b,cx).is_err());
 }

 #[test]
 fn interpreted_public_type_mutation_clone_and_dump_boundaries() {
  use crate::core::env::{Env,sym};
  let roots=RootSet::default(); let cx=&mut Context::new(&roots);
  sym::init_symbols();
  root!(env,init(Env::new_isolated(cx).unwrap()),cx);
  let body=Object::from(Cons::new(NIL,NIL,&cx.block));
  // SAFETY: sameowner field graphs; env publication roots value before eval/GC.
  let object=Object::from(unsafe {InterpretedFn::new(NIL,body,NIL,NIL,None,cx).unwrap()});
  assert!(super::super::validate_function_clone(object).is_err());
  assert!(!crate::data::vectorp(object));
  assert!(crate::data::aset(object,0,7.into()).is_err());
  assert!(crate::data::aref(object,0,cx).unwrap().is_nil());
  env.set_var(crate::lread::intern("numeric-recovered-interpreted",cx),object).unwrap();
  let form=crate::reader::read("(list (type-of numeric-recovered-interpreted) (cl-type-of numeric-recovered-interpreted) (vectorp numeric-recovered-interpreted))",cx).unwrap().0;
  root!(form,form,cx);
  let result=crate::interpreter::eval(form,None,env,cx).unwrap();
  assert_eq!(result.to_string(),"(interpreted-function interpreted-function nil)");
  let directory=std::env::temp_dir().join(format!("rune-interpreted-dump-{}-{}",std::process::id(),cx.domain_id()));
  // Exclusive creation: never reuse/remove a preexisting directory or symlink.
  std::fs::create_dir(&directory).unwrap();
  let path=directory.join("existing.dump"); std::fs::write(&path,b"preserved image").unwrap();
  let result=crate::dump::dump_to_file(&path,env,cx);
  let bytes=std::fs::read(&path).unwrap();
  std::fs::remove_file(&path).unwrap(); std::fs::remove_dir(&directory).unwrap();
  assert_eq!(result.unwrap_err().kind(),std::io::ErrorKind::Unsupported);
  assert_eq!(bytes,b"preserved image");
 }

 #[test]
 fn interpreted_native_equal_key_validation_rejects_root_and_nested() {
  use super::super::{hash_key::validate_keys,HashTableTest};
  let roots=RootSet::default(); let cx=&Context::new(&roots);
  let body=Object::from(Cons::new(NIL,NIL,&cx.block));
  // SAFETY: current-owner graph, shared Context excludes collection/callbacks.
  let value=Object::from(unsafe {InterpretedFn::new(NIL,body,NIL,NIL,None,cx).unwrap()});
  let nested=cx.add(&[value][..]);
  assert!(validate_keys(HashTableTest::Equal,[value]).is_err());
  assert!(validate_keys(HashTableTest::Equal,[nested]).is_err());
  assert!(validate_keys(HashTableTest::Eq,[value]).is_ok());
  assert!(validate_keys(HashTableTest::Eql,[value]).is_ok());
 }

}
