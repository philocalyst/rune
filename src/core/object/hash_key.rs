//! Real strong hash tests. Native trait callbacks never evaluate Lisp or collect.
use super::{ObjCell,Object,ObjectType};
use crate::core::gc::{GcState,Trace};
use anyhow::{Result,ensure};
use num_traits::ToPrimitive;
use std::hash::{Hash,Hasher};
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub(crate) enum HashTableTest {Eq,Eql,Equal}
impl HashTableTest {pub(crate) fn name(self)->&'static str {match self{Self::Eq=>"eq",Self::Eql=>"eql",Self::Equal=>"equal"}}}
pub(super) struct SemanticKey {pub value:ObjCell,pub test:HashTableTest}
impl SemanticKey {
 /// Caller excludes collection and retains the source graph until publication
 /// into a traced owning table. Destination matching is not source provenance.
 pub unsafe fn new(value:Object,test:HashTableTest)->Self {Self{value:unsafe{ObjCell::new(value)},test}}
 pub fn trace(&self,state:&mut GcState){self.value.trace(state);}
}
fn eql(a:Object,b:Object)->bool {
 if a.ptr_eq(b){return true}
 match(a.untag(),b.untag()){
  (ObjectType::Float(a),ObjectType::Float(b))=>a.to_bits()==b.to_bits(),
  (ObjectType::BigInt(a),ObjectType::BigInt(b))=>**a==**b,
  (ObjectType::Int(a),ObjectType::BigInt(b))|(ObjectType::BigInt(b),ObjectType::Int(a))=>b.to_i64()==Some(a),
  _=>false,
 }
}
enum TextUnits<'a>{Text(std::str::Chars<'a>),Bytes(std::slice::Iter<'a,u8>)}
impl Iterator for TextUnits<'_>{type Item=u32;fn next(&mut self)->Option<u32>{match self{Self::Text(c)=>c.next().map(|c|c as u32),Self::Bytes(b)=>b.next().map(|b|if *b<128{u32::from(*b)}else{0x110000+u32::from(*b)})}}}
fn text_units(o:Object)->Option<TextUnits<'_>>{match o.untag(){ObjectType::String(s)=>Some(TextUnits::Text(s.chars())),ObjectType::ByteString(s)=>Some(TextUnits::Bytes(s.iter())),_=>None}}
fn equal(a:Object,b:Object)->bool {
 let mut tasks=vec![(a,b,0usize)];let mut work=0usize;
 while let Some((a,b,depth))=tasks.pop(){
  work+=1;if work>100000||depth>256{return false} if eql(a,b){continue}
  match(a.untag(),b.untag()){
   (ObjectType::String(_)|ObjectType::ByteString(_),ObjectType::String(_)|ObjectType::ByteString(_))=>if !text_units(a).unwrap().eq(text_units(b).unwrap()){return false},
   (ObjectType::Cons(a),ObjectType::Cons(b))=>{tasks.push((a.cdr(),b.cdr(),depth+1));tasks.push((a.car(),b.car(),depth+1));},
   (ObjectType::Vec(a),ObjectType::Vec(b))=>{if a.len()!=b.len(){return false}for(a,b)in a.iter().zip(b.iter()){tasks.push((a.get(),b.get(),depth+1));}},
   (ObjectType::Record(a),ObjectType::Record(b))=>{if a.len()!=b.len(){return false}for(a,b)in a.iter().zip(b.iter()){tasks.push((a.get(),b.get(),depth+1));}},
   (ObjectType::ByteFn(a),ObjectType::ByteFn(b))=>{if a.args!=b.args||a.depth!=b.depth||a.codes()!=b.codes()||a.consts().len()!=b.consts().len(){return false}for(a,b)in a.consts().iter().zip(b.consts()){tasks.push((*a,*b,depth+1));}},
   (ObjectType::Cons(_)|ObjectType::Vec(_)|ObjectType::Record(_)|ObjectType::ByteFn(_),_)=>return false,
   _=>if a!=b{return false},
  }
  if tasks.len()>100000{return false}
 }
 true
}
impl PartialEq for SemanticKey {fn eq(&self,b:&Self)->bool{self.test==b.test&&match self.test{HashTableTest::Eq=>self.value.get().ptr_eq(b.value.get()),HashTableTest::Eql=>eql(self.value.get(),b.value.get()),HashTableTest::Equal=>equal(self.value.get(),b.value.get())}}}
impl Eq for SemanticKey{}
fn number_hash<H:Hasher>(o:Object,h:&mut H)->bool {match o.untag(){
 ObjectType::Int(n)=>{0u8.hash(h);n.hash(h);true},
 ObjectType::BigInt(n)=>{if let Some(n)=n.to_i64(){0u8.hash(h);n.hash(h)}else{1u8.hash(h);(**n).hash(h)}true},
 ObjectType::Float(n)=>{2u8.hash(h);n.to_bits().hash(h);true},_=>false,
}}
impl Hash for SemanticKey {fn hash<H:Hasher>(&self,h:&mut H){
 self.test.name().hash(h);let o=self.value.get();
 match self.test {
  HashTableTest::Eq=>o.into_raw().addr().hash(h),
  HashTableTest::Eql=>if !number_hash(o,h){3u8.hash(h);o.into_raw().addr().hash(h)},
  HashTableTest::Equal=>{
   if number_hash(o,h){return}
   if let Some(units)=text_units(o){4u8.hash(h);for unit in units{unit.hash(h)}return}
   if matches!(o.untag(),ObjectType::Symbol(_)){5u8.hash(h);o.into_raw().addr().hash(h);return}
   // Structural/opaque type hashes deliberately do not traverse the graph.
   // Rehash during GC cannot recurse through mutated cyclic structural keys.
   6u8.hash(h);std::mem::discriminant(&o.untag()).hash(h);
  }
 }
}}
const MAX_WORK:usize=100000;const MAX_PAYLOAD:usize=1024*1024;const MAX_TOTAL:usize=16*1024*1024;
enum Visit<'a>{Enter(Object<'a>,usize),Leave(usize)}
/// Validate all resident keys AND the query in a single aggregate budget before
/// any trait comparison. Repeated DAG expansion is charged, not memoized away.
pub(super) fn validate_keys<'a>(test:HashTableTest,keys:impl IntoIterator<Item=Object<'a>>)->Result<()> {
 if test==HashTableTest::Eq{return Ok(())}
 let mut active=std::collections::HashSet::new();let mut work=0usize;let mut bytes=0usize;
 for key in keys {
  let mut tasks=vec![Visit::Enter(key,0)];
  while let Some(task)=tasks.pop(){
   let (o,depth)=match task{Visit::Leave(id)=>{active.remove(&id);continue},Visit::Enter(o,d)=>(o,d)};
   work=work.checked_add(1).ok_or_else(||anyhow::anyhow!("Hash key work overflow"))?;
   ensure!(work<=MAX_WORK&&depth<=256,"Hash key traversal limit exceeded");
   let payload=match o.untag(){
    ObjectType::String(s) if test==HashTableTest::Equal=>s.as_bytes().len(),
    ObjectType::ByteString(s) if test==HashTableTest::Equal=>s.len(),
    ObjectType::ByteFn(f) if test==HashTableTest::Equal=>f.codes().len(),
    ObjectType::BigInt(n)=>usize::try_from(n.bits()/8+u64::from(n.bits()%8!=0))?,
    _=>0,
   };
   ensure!(payload<=MAX_PAYLOAD,"Hash key payload limit exceeded");
   bytes=bytes.checked_add(payload).ok_or_else(||anyhow::anyhow!("Hash key payload overflow"))?;
   ensure!(bytes<=MAX_TOTAL,"Hash key aggregate payload limit exceeded");
   if test==HashTableTest::Eql{continue}
   let mut children=Vec::new();
   match o.untag(){
    ObjectType::InterpretedFn(_)=>anyhow::bail!("Structural interpreted function keys are unsupported"),
    ObjectType::Marker(_)=>anyhow::bail!("Structural Marker hash keys need owner context"),
    ObjectType::Cons(c)=>children.extend([c.car(),c.cdr()]),
    ObjectType::Vec(v)=>{ensure!(v.len()<=MAX_WORK-work,"Hash key edge limit exceeded");children.try_reserve_exact(v.len())?;children.extend(v.iter().map(|c|c.get()));},
    ObjectType::Record(v)=>{ensure!(v.len()<=MAX_WORK-work,"Hash key edge limit exceeded");children.try_reserve_exact(v.len())?;children.extend(v.iter().map(|c|c.get()));},
    ObjectType::ByteFn(f)=>{ensure!(f.consts().len()<=MAX_WORK-work,"Hash key edge limit exceeded");children.try_reserve_exact(f.consts().len())?;children.extend(f.consts().iter().copied());},
    _=>{},
   }
   if !children.is_empty(){let id=o.into_raw().addr();ensure!(active.insert(id),"Cyclic equal hash keys are unsupported");ensure!(tasks.len().checked_add(children.len()+1).is_some_and(|n|n<=MAX_WORK),"Hash key pending limit exceeded");tasks.try_reserve(children.len()+1)?;tasks.push(Visit::Leave(id));for child in children.into_iter().rev(){tasks.push(Visit::Enter(child,depth+1));}}
  }
 }
 Ok(())
}
/// Reject identity changes during cross-arena copying, including transitive
/// opaque identity leaves of Equal keys. A real owner-runtime design is needed
/// for the known cl-generic global Eql cache / Unmoved Cons insertion failure.
pub(super) fn ensure_portable_key(test:HashTableTest,key:Object)->Result<()> {
 validate_keys(test,std::iter::once(key))?;
 let mut pending=vec![(key,0usize)];let mut work=0usize;
 while let Some((key,depth))=pending.pop(){work+=1;ensure!(work<=MAX_WORK&&depth<=256,"Hash key transfer traversal limit exceeded");
  if test!=HashTableTest::Equal {
   let supported=matches!(key.untag(),ObjectType::Int(_)|ObjectType::SubrFn(_))||matches!(key.untag(),ObjectType::Symbol(s)if s.interned())||test==HashTableTest::Eql&&matches!(key.untag(),ObjectType::Float(_)|ObjectType::BigInt(_));
   ensure!(supported,"Identity-sensitive hash key transfer is unsupported");continue;
  }
  match key.untag(){
   ObjectType::HashTable(_)|ObjectType::CharTable(_)|ObjectType::ChannelSender(_)|ObjectType::ChannelReceiver(_)=>anyhow::bail!("Opaque identity hash key transfer is unsupported"),
   ObjectType::Symbol(s)if !s.interned()=>anyhow::bail!("Uninterned symbol hash key transfer is unsupported"),
   ObjectType::Cons(c)=>{pending.push((c.car(),depth+1));pending.push((c.cdr(),depth+1));},
   ObjectType::Vec(v)=>for child in v.iter(){pending.push((child.get(),depth+1));},
   ObjectType::Record(v)=>for child in v.iter(){pending.push((child.get(),depth+1));},
   ObjectType::ByteFn(f)=>for child in f.consts(){pending.push((*child,depth+1));},
   _=>{},
  }
  ensure!(pending.len()<=MAX_WORK,"Hash key transfer pending limit exceeded");
 }
 Ok(())
}
