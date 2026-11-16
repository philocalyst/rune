//! Reconstructed native seed charset foundation. No map, unification or codec shim.
//! GNU30 charset.c native seed order and plist fields, not dumped Lisp state.
use crate::core::{cons::Cons, env::{Env, intern}, gc::{Context, Rt}, object::{Object, ObjectType, NIL, TRUE}};
use rune_macros::defun;
use rune_core::macros::list;
use anyhow::Result;

defvar!(CHARSET_LIST, crate::charset::seed_list(cx));
const NAMES: [&str;5] = ["ascii","iso-8859-1","unicode","emacs","eight-bit"];

/// No Lisp edges: copied geometry and stable numeric ID authority only.
#[derive(Debug, Clone)]
pub(crate) struct OffsetDescriptor {
 space: [[u8;2];4], min:u32, max:u32, offset:u32, invalid:u32, ascii:bool,
}
impl OffsetDescriptor {
 fn rank(&self,code:u32)->Option<u64>{let mut rank=0u64;let mut stride=1u64;for i in 0..4{let byte=((code>>(8*i))&255)as u8;let [lo,hi]=self.space[i];if byte<lo||byte>hi{return None;}rank+=u64::from(byte-lo)*stride;stride*=u64::from(hi-lo)+1;}Some(rank)}
 fn decode(&self,code:u32)->Option<u32>{if code<128&&self.ascii{return Some(code);}if code<self.min||code>self.max{return None;}let index=self.rank(code)?.checked_sub(self.rank(self.min)?)?;u32::try_from(index+u64::from(self.offset)).ok().filter(|c|*c<=0x3fffff)}
 fn encode(&self,ch:u32)->Option<u32>{let code=if ch<128&&self.ascii{ch}else{let index=ch.checked_sub(self.offset)?;let mut rank=u64::from(index)+self.rank(self.min)?;let mut code=0u32;for i in 0..4{let [lo,hi]=self.space[i];let size=u64::from(hi-lo)+1;code|=((rank%size)as u32+u32::from(lo))<<(8*i);rank/=size;}if rank!=0||code<self.min||code>self.max{return None;}code};(code!=self.invalid).then_some(code)}
}
fn seed_descriptor(id:usize)->OffsetDescriptor{let (space,min,max,offset)=match id{
 0=>([[0,127],[0,0],[0,0],[0,0]],0,127,0),
 1=>([[0,255],[0,0],[0,0],[0,0]],0,255,0),
 2=>([[0,255],[0,255],[0,16],[0,0]],0,0x10ffff,0),
 3=>([[0,255],[0,255],[0,63],[0,0]],0,0x3fff7f,0),
 _=>([[128,255],[0,0],[0,0],[0,0]],128,255,0x3fff80)};
 OffsetDescriptor{space,min,max,offset,invalid:if min>0{0}else{max+1},ascii:id!=4}}

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
  let attrs:Object=cx.add(vec![Object::from(id as i64),plist]).into();self.charset_registry.insert(key,attrs);self.charset_offsets.push(seed_descriptor(id));
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

fn code_u32(value:Object,_cx:&Context)->Result<u32>{
 if let ObjectType::Int(n)=value.untag(){if let Ok(n)=u32::try_from(n){return Ok(n);}}
 anyhow::bail!("Unsupported charset code-point; expected unsigned32 integer: {}",value)
}
fn descriptor<'e>(charset:Object,env:&'e Rt<Env>,cx:&Context)->Result<&'e OffsetDescriptor>{let value=attrs(charset,env,cx)?;let ObjectType::Vec(v)=value.untag()else{unreachable!()};let ObjectType::Int(id)=v[0].get().untag()else{unreachable!()};env.charset_offsets.get(id as usize).ok_or_else(||anyhow::anyhow!("Missing charset descriptor"))}
#[defun]
fn decode_char<'c>(charset:Object<'c>,code:Object<'c>,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>>{let desc=descriptor(charset,env,cx)?;let code=code_u32(code,cx)?;Ok(desc.decode(code).map_or(NIL,|c|Object::from(i64::from(c))))}
#[defun]
fn encode_char<'c>(ch:Object<'c>,charset:Object<'c>,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>>{let desc=descriptor(charset,env,cx)?;let ObjectType::Int(n)=ch.untag()else{return Err(crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),list!(intern("characterp",cx),ch;cx),cx).into())};if !(0..=0x3fffff).contains(&n){return Err(crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),list!(intern("characterp",cx),ch;cx),cx).into())}Ok(desc.encode(n as u32).map_or(NIL,|c|Object::from(i64::from(c))))}

// Explicitly bounded symbol-only alias prerequisite. This is not yet full GNU
// arbitrary-key alias support or dynamic charset definition support.
const CHARSET_REGISTRY_KEY_LIMIT: usize = 4096;
const CHARSET_ALIAS_PUBLICATION_LIMIT: usize = 65536;

#[defun]
fn define_charset_alias<'c>(alias: Object<'c>, charset: Object<'c>, env: &mut Rt<Env>, cx: &'c Context) -> Result<()> {
    // GNU validates the target first. Keep Object here so generated argument
    // conversion cannot prematurely reject the alias before target validation.
    let target = attrs(charset, env, cx)?;
    let ObjectType::Symbol(alias) = alias.untag() else {
        anyhow::bail!("Unsupported nonsymbol charset alias in bounded registry");
    };
    let is_new = env.charset_registry.get(alias).is_none();
    if is_new && env.charset_registry.iter().count() >= CHARSET_REGISTRY_KEY_LIMIT {
        anyhow::bail!("Charset registry key limit exceeded");
    }
    if env.charset_alias_publications >= CHARSET_ALIAS_PUBLICATION_LIMIT {
        anyhow::bail!("Charset alias publication limit exceeded");
    }
    // No traversal of a caller-mutated/cyclic public list. Reserve table growth
    // before visible publication; Lisp allocations under shared Context cannot
    // trigger collection. A process-level allocation failure is not recoverable.
    env.charset_registry.try_reserve(usize::from(is_new))?;
    let list_symbol = intern("charset-list", cx);
    let list_missing = env.vars.get(list_symbol).is_none();
    env.vars.try_reserve(usize::from(list_missing))?;
    let old_list = env.vars.get(list_symbol).map_or(NIL, |slot| slot.bind(cx));
    let new_list: Object = Cons::new(alias, old_list, cx).into();
    // No callbacks, GC, or fallible work in the publication sequence.
    env.charset_registry.insert(alias, target);
    env.vars.insert(list_symbol, new_list);
    env.charset_alias_publications += 1;
    Ok(())
}

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

 #[test]
 fn seed_offset_boundaries_and_sparse_arithmetic(){
  for id in 0..5{let d=seed_descriptor(id);assert_eq!(d.decode(d.min),Some(d.offset));assert_eq!(d.encode(d.offset),Some(d.min));let last=d.decode(d.max).unwrap();assert_eq!(d.encode(last),Some(d.max));assert_eq!(d.decode(d.max+1),None);}
  assert_eq!(seed_descriptor(4).decode(128),Some(0x3fff80));assert_eq!(seed_descriptor(3).decode(0x3fff80),None);
  let sparse=OffsetDescriptor{space:[[32,33],[1,2],[0,0],[0,0]],min:0x120,max:0x221,offset:1000,invalid:0,ascii:false};
  for (code,ch) in [(0x120,1000),(0x121,1001),(0x220,1002),(0x221,1003)]{assert_eq!(sparse.decode(code),Some(ch));assert_eq!(sparse.encode(ch),Some(code));}assert_eq!(sparse.decode(0x122),None);assert_eq!(sparse.encode(1004),None);
 }
 #[test]
 fn clipped_geometry_ascii_and_invalid_sentinel(){
  let clipped=OffsetDescriptor{space:[[32,33],[1,2],[0,0],[0,0]],min:0x121,max:0x220,offset:1000,invalid:0,ascii:false};
  assert_eq!(clipped.decode(0x120),None);assert_eq!(clipped.decode(0x121),Some(1000));assert_eq!(clipped.decode(0x220),Some(1001));assert_eq!(clipped.decode(0x221),None);
  assert_eq!(clipped.encode(999),None);assert_eq!(clipped.encode(1000),Some(0x121));assert_eq!(clipped.encode(1001),Some(0x220));assert_eq!(clipped.encode(1002),None);
  let shortcut=OffsetDescriptor{ascii:true,invalid:65,..clipped};
  assert_eq!(shortcut.decode(65),Some(65));assert_eq!(shortcut.encode(64),Some(64));assert_eq!(shortcut.encode(65),None);assert_eq!(shortcut.decode(128),None);
 }
 #[test]
 fn offset_mapping_ignores_public_plist_and_validates_in_native_order(){
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  let count=env.charset_offsets.len();assert_eq!(count,5);
  crate::lread::load_internal("(set-charset-plist 'eight-bit '(:code-offset 0 :code-space [0 0]))",cx,env).unwrap();
  for _ in 0..4{env.initialize_condition_metadata(cx);cx.garbage_collect(true);assert_eq!(env.charset_offsets.len(),count);assert_eq!(decode_char(intern("eight-bit",cx).into(),128.into(),env,cx).unwrap().to_string(),"4194176");assert_eq!(encode_char(4194176.into(),intern("eight-bit",cx).into(),env,cx).unwrap().to_string(),"128");}
  for (input,expected) in [("(encode-char -1 'ascii)","(wrong-type-argument characterp -1)"),("(encode-char 4194304 'ascii)","(wrong-type-argument characterp 4194304)"),("(encode-char -1 'unknown-offset-charset)","(wrong-type-argument charsetp unknown-offset-charset)"),("(decode-char 'unknown-offset-charset -1)","(wrong-type-argument charsetp unknown-offset-charset)")]{let source=format!("(condition-case e {input} (error e))");let form=crate::reader::read(&source,cx).unwrap().0;root!(form,cx);assert_eq!(crate::interpreter::eval(form,None,env,cx).unwrap().to_string(),expected);}
  assert_eq!(encode_char(4194303.into(),intern("eight-bit",cx).into(),env,cx).unwrap().to_string(),"255");
 }
 #[test]
 fn seed_offset_public_paths_match_gnu(){
  let source="(list (decode-char 'ascii 127) (decode-char 'ascii 128) (encode-char 255 'iso-8859-1) (decode-char 'unicode 1114111) (decode-char 'unicode 1114112) (decode-char 'emacs 4194175) (decode-char 'emacs 4194176) (decode-char 'eight-bit 128) (encode-char 4194176 'eight-bit))";
  // Use the existing genuine GNU oracle (batch in the qualification runner),
  // but initialize Rune through its normal native variable setup. assert_elprop
  // intentionally creates only a bare Env, with an empty charset registry.
  crate::library::elprop::start_emacs().unwrap();
  let native=crate::library::elprop::eval(source).unwrap();
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);
  assert_eq!(env.charset_registry.iter().count(),0);
  crate::core::env::init_variables(cx,env);
  assert_eq!(env.charset_registry.iter().count(),5);
  let form=crate::reader::read(source,cx).unwrap().0;root!(form,cx);
  let actual=crate::interpreter::eval(form,None,env,cx).unwrap().to_string();
  assert_eq!(actual,native,"Rune differs from fresh GNU observation");
 }

 #[test]
 fn alias_identity_retarget_and_duplicate_publication() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(progn (define-charset-alias 'rune-alias 'ascii) (if (eq (charset-plist 'rune-alias) (charset-plist 'ascii)) nil (error \"alias copied plist\")) (define-charset-alias 'rune-alias 'eight-bit) (if (= (decode-char 'rune-alias 128) 4194176) nil (error \"alias retarget failed\")) (define-charset-alias 'rune-alias 'eight-bit) (if (equal (list (car charset-list) (car (cdr charset-list)) (car (cdr (cdr charset-list)))) '(rune-alias rune-alias rune-alias)) nil (error \"duplicate alias list insertion missing\")))",cx,env).unwrap();
  assert_eq!(env.charset_offsets.len(),5);
  assert_eq!(env.charset_alias_publications,3);
 }
 #[test]
 fn alias_target_first_rejection_and_limits_are_atomic() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  let form=crate::reader::read("(condition-case e (define-charset-alias 42 'no-such-charset) (error e))",cx).unwrap().0;root!(form,cx);
  assert_eq!(crate::interpreter::eval(form,None,env,cx).unwrap().to_string(),"(wrong-type-argument charsetp no-such-charset)");
  let list_symbol=intern("charset-list",cx);let old=env.vars.get(list_symbol).unwrap().bind(cx);root!(old,cx);
  assert!(define_charset_alias(42.into(),intern("ascii",cx).into(),env,cx).is_err());
  assert_eq!(env.charset_alias_publications,0);
  assert!(env.vars.get(list_symbol).unwrap().bind(cx).ptr_eq(old.bind(cx)));
  env.charset_alias_publications=CHARSET_ALIAS_PUBLICATION_LIMIT;
  assert!(define_charset_alias(intern("blocked-alias",cx).into(),intern("ascii",cx).into(),env,cx).is_err());
  assert!(!charsetp(intern("blocked-alias",cx).into(),env));
  assert!(env.vars.get(list_symbol).unwrap().bind(cx).ptr_eq(old.bind(cx)));
 }
 #[test]
 fn alias_registry_retains_uninterned_name_cycle_and_owner_isolation() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(let ((name (make-symbol \"private-alias\")) (cycle (cons 42 nil))) (setcdr cycle cycle) (set-charset-plist 'ascii cycle) (define-charset-alias name 'ascii) (setq charset-list nil))",cx,env).unwrap();
  // Independently root a second uninterned key so each moving collection must
  // repair both the root and registry hash index, not merely retain entries.
  let rooted_name:Object=crate::core::object::Symbol::new_uninterned("rooted-private-alias",cx).into();root!(rooted_name,cx);
  define_charset_alias(rooted_name.bind(cx),intern("ascii",cx).into(),env,cx).unwrap();
  for _ in 0..4 {
   env.initialize_condition_metadata(cx);cx.garbage_collect(true);
   assert!(charsetp(rooted_name.bind(cx),env));
   assert_eq!(charset_id_internal(Some(rooted_name.bind(cx)),env,cx).unwrap().to_string(),"0");
   assert!(charset_plist(rooted_name.bind(cx),env,cx).unwrap().ptr_eq(charset_plist(intern("ascii",cx).into(),env,cx).unwrap()));
   assert!(!charsetp(intern("rooted-private-alias",cx).into(),env));
  }
  assert_eq!(env.charset_registry.iter().count(),7);
  assert!(!charsetp(intern("private-alias",cx).into(),env));
  let ascii=attrs(intern("ascii",cx).into(),env,cx).unwrap();
  assert_eq!(env.charset_registry.iter().filter(|(_,v)|v.ptr_eq(ascii)).count(),3);
  let plist=charset_plist(intern("ascii",cx).into(),env,cx).unwrap();let ObjectType::Cons(pair)=plist.untag()else{panic!("lost cyclic plist")};assert!(pair.cdr().ptr_eq(plist));
  root!(other,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,other);
  assert_eq!(other.charset_registry.iter().count(),5);assert_eq!(other.charset_alias_publications,0);
  assert!(!charsetp(rooted_name.bind(cx),other));
 }

 #[test]
 fn alias_key_capacity_allows_retarget_but_rejects_new_key() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  let ascii=intern("ascii",cx).into();
  let shared=attrs(ascii,env,cx).unwrap();
  // Fill native table directly to isolate admission boundary without thousands
  // of public Lisp allocations. This is not an end-to-end capacity oracle.
  for _ in 5..CHARSET_REGISTRY_KEY_LIMIT {
   let name=crate::core::object::Symbol::new_uninterned("capacity-alias",cx);
   env.charset_registry.insert(name,shared);
  }
  assert_eq!(env.charset_registry.iter().count(),CHARSET_REGISTRY_KEY_LIMIT);
  let list_symbol=intern("charset-list",cx);let old=env.vars.get(list_symbol).unwrap().bind(cx);root!(old,cx);
  assert!(define_charset_alias(intern("overflow-alias",cx).into(),ascii,env,cx).is_err());
  assert!(!charsetp(intern("overflow-alias",cx).into(),env));
  assert!(env.vars.get(list_symbol).unwrap().bind(cx).ptr_eq(old.bind(cx)));
  define_charset_alias(ascii,intern("eight-bit",cx).into(),env,cx).unwrap();
  assert_eq!(charset_id_internal(Some(ascii),env,cx).unwrap().to_string(),"4");
  assert_eq!(env.charset_registry.iter().count(),CHARSET_REGISTRY_KEY_LIMIT);
  assert_eq!(env.charset_alias_publications,1);
 }
 #[test]
 fn alias_prepends_without_traversing_cyclic_public_list() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(let ((cycle (cons 17 nil))) (setcdr cycle cycle) (setq charset-list cycle) (define-charset-alias 'cycle-alias 'ascii) (if (and (eq (car charset-list) 'cycle-alias) (eq (cdr charset-list) cycle) (eq (cdr cycle) cycle)) nil (error \"alias traversed or copied list\")))",cx,env).unwrap();
 }

}
