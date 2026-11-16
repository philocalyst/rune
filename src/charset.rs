//! Reconstructed native seed charset foundation. No map, unification or codec shim.
//! GNU30 charset.c native seed order and plist fields, not dumped Lisp state.
use crate::core::{cons::Cons, env::{Env, intern}, gc::{Context, Rt}, object::{Object, ObjectType, NIL, TRUE}};
use rune_macros::defun;
use rune_core::macros::list;
use anyhow::Result;

defvar!(CHARSET_LIST, crate::charset::seed_list(cx));
const NAMES: [&str;5] = ["ascii","iso-8859-1","unicode","emacs","eight-bit"];

pub(crate) fn seed_list<'c>(cx:&'c Context)->Object<'c>{
 let mut result=NIL;for name in NAMES{result=Cons::new(intern(name,cx),result,cx).into();}result
}
impl crate::core::env::RootedEnv<'_> {
pub(crate) fn initialize_charset_registry(&mut self,cx:&Context){
 if self.charset_seeded{return;}
 for (id,name) in NAMES.into_iter().enumerate(){
  let key=intern(name,cx);let dimension=if id==2||id==3{3}else{1};
  let bytes:[i64;8]=match id{0=>[0,127,0,0,0,0,0,0],1=>[0,255,0,0,0,0,0,0],2=>[0,255,0,255,0,16,0,0],3=>[0,255,0,255,0,63,0,0],_=>[128,255,0,0,0,0,0,0]};
  let space:Object=cx.add(bytes.into_iter().map(Object::from).collect::<Vec<_>>()).into();
  let final_char=if id==0{Object::from(66)}else{NIL};let mule=if id==0{Object::from(0)}else{NIL};let ascii=if id==4{NIL}else{TRUE};let offset=if id==4{0x3fff80}else{0};
  let plist=list!(intern(":name",cx),key,intern(":dimension",cx),dimension,intern(":code-space",cx),space,intern(":iso-final-char",cx),final_char,intern(":emacs-mule-id",cx),mule,intern(":ascii-compatible-p",cx),ascii,intern(":code-offset",cx),offset;cx);
  let attrs:Object=cx.add(vec![Object::from(id as i64),plist]).into();self.charset_registry.insert(key,attrs);
 }self.charset_seeded=true;
}
}
fn attrs<'c>(charset:Object<'c>,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>>{
 if let ObjectType::Symbol(name)=charset.untag(){if let Some(value)=env.charset_registry.get(name){return Ok(value.bind(cx));}}
 Err(crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),list!(intern("charsetp",cx),charset;cx),cx).into())
}
#[defun]
fn charsetp(charset:Object,env:&Rt<Env>)->bool{matches!(charset.untag(),ObjectType::Symbol(name) if env.charset_registry.get(name).is_some())}
#[defun]
fn charset_id_internal<'c>(charset:Option<Object<'c>>,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>>{let value=attrs(charset.unwrap_or(NIL),env,cx)?;let ObjectType::Vec(v)=value.untag()else{unreachable!()};Ok(v[0].get())}
#[defun]
fn charset_plist<'c>(charset:Object<'c>,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>>{let value=attrs(charset,env,cx)?;let ObjectType::Vec(v)=value.untag()else{unreachable!()};Ok(v[1].get())}
#[defun]
fn set_charset_plist<'c>(charset:Object<'c>,plist:Object<'c>,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>>{let value=attrs(charset,env,cx)?;let ObjectType::Vec(v)=value.untag()else{unreachable!()};v.try_mut()?[1].set(plist);Ok(plist)}

#[cfg(test)]
mod tests {
 use super::*;
 use crate::core::gc::RootSet;
 use rune_core::macros::root;
 #[test]
 fn native_seed_ids_plist_and_owner_isolation(){
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();
  root!(env,init(crate::core::env::Env::new_isolated(cx).unwrap()),cx);
  env.initialize_charset_registry(cx);
  for (id,name) in NAMES.into_iter().enumerate(){let key=intern(name,cx).into();assert!(charsetp(key,env));assert_eq!(charset_id_internal(Some(key),env,cx).unwrap().to_string(),id.to_string());}
  assert_eq!(seed_list(cx).to_string(),"(eight-bit emacs unicode iso-8859-1 ascii)");
  let ascii=intern("ascii",cx).into();let old=charset_plist(ascii,env,cx).unwrap();root!(old,cx);
  assert_eq!(old.bind(cx).to_string(),"(:name ascii :dimension 1 :code-space [0 127 0 0 0 0 0 0] :iso-final-char 66 :emacs-mule-id 0 :ascii-compatible-p t :code-offset 0)");
  let replacement=Cons::new(42,NIL,cx).into();set_charset_plist(ascii,replacement,env,cx).unwrap();
  for _ in 0..4{cx.garbage_collect(true);assert_eq!(charset_plist(intern("ascii",cx).into(),env,cx).unwrap().to_string(),"(42)");}
  root!(other,init(crate::core::env::Env::new_isolated(cx).unwrap()),cx);other.initialize_charset_registry(cx);assert!(charset_plist(intern("ascii",cx).into(),other,cx).unwrap().to_string().starts_with("(:name ascii"));
 }
 #[test]
 fn public_entry_initializes_once_and_registry_traces_cycle(){
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);
  crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(if (and (charsetp 'ascii) (= (charset-id-internal 'eight-bit) 4) (equal charset-list '(eight-bit emacs unicode iso-8859-1 ascii))) nil (error \"native charset seeds missing\"))",cx,env).unwrap();
  crate::lread::load_internal("(let ((cycle (cons 42 nil))) (setcdr cycle cycle) (set-charset-plist 'ascii cycle))",cx,env).unwrap();
  env.initialize_condition_metadata(cx);
  for _ in 0..4{cx.garbage_collect(true);let value=charset_plist(intern("ascii",cx).into(),env,cx).unwrap();let ObjectType::Cons(pair)=value.untag()else{panic!("registry lost plist")};assert_eq!(pair.car().to_string(),"42");assert!(pair.cdr().ptr_eq(value));}
  crate::lread::load_internal("(if (eq (set-charset-plist 'ascii 42) 42) nil (error \"setter did not return arbitrary value\"))",cx,env).unwrap();env.initialize_condition_metadata(cx);assert_eq!(charset_plist(intern("ascii",cx).into(),env,cx).unwrap().to_string(),"42");
 }
 #[test]
 fn charset_designators_use_identity_and_exact_type_conditions(){
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  for (input,expected) in [("(charset-id-internal)","(wrong-type-argument charsetp nil)"),("(charset-plist 42)","(wrong-type-argument charsetp 42)"),("(charset-plist (make-symbol \"ascii\"))","(wrong-type-argument charsetp ascii)")]{
   let source=format!("(condition-case e {input} (error e))");let form=crate::reader::read(&source,cx).unwrap().0;root!(form,cx);assert_eq!(crate::interpreter::eval(form,None,env,cx).unwrap().to_string(),expected);
  }
  crate::lread::load_internal("(if (or (charsetp nil) (charsetp 42) (charsetp (make-symbol \"ascii\"))) (error \"name-based charset authority\") nil)",cx,env).unwrap();
 }

}
