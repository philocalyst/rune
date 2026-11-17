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
  let attrs:Object=cx.add(vec![Object::from(id as i64),plist]).into();self.charset_registry.insert(key,attrs);self.charset_offsets.push(seed_descriptor(id));self.charset_names.push(key);self.charset_attributes.push(attrs);
 }self.charset_registration = RegistrationState::seeds();self.charset_seeded=true;
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

// Owner-local native registration authority. No Lisp edges in this state;
// canonical names live in Env.charset_names and are traced independently.
#[derive(Debug, Clone)]
pub(crate) struct RegistrationMeta {
 dimension: u8, iso_final: Option<u8>, iso_revision: i8,
 mule_id: Option<u8>, supplementary: bool,
}
#[derive(Debug, Default)]
pub(crate) struct RegistrationState {
 meta: Vec<RegistrationMeta>, priority: Vec<usize>,
 iso: Vec<Option<usize>>, mule: Vec<Option<(usize,u8)>>,
 iso_members: Vec<usize>, mule_members: Vec<usize>,
 tick: u64, publications: usize,
}
impl RegistrationState {
 fn seeds()->Self {
  let mut s=Self {meta:(0..5).map(|id| RegistrationMeta {
   dimension:if id==2||id==3{3}else{1},iso_final:if id==0{Some(66)}else{None},
   iso_revision:if id<2{-1}else{0},mule_id:if id==0{Some(0)}else{None},supplementary:id>=3,
  }).collect(),priority:(0..5).collect(),iso:vec![None;3*2*128],mule:vec![None;256],
   iso_members:vec![0],mule_members:vec![0],tick:5,publications:0};
  s.iso[66]=Some(0);s.mule[0]=Some((0,2));s
 }
 fn iso_index(dimension:u8,chars96:bool,final_char:u8)->usize {
  (usize::from(dimension)-1)*256+usize::from(chars96)*128+usize::from(final_char)
 }
}
fn registration_type<'c>(value:Object<'c>,predicate:&str,cx:&'c Context)->anyhow::Error {
 crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),list!(intern(predicate,cx),value;cx),cx).into()
}
fn registration_int(value:Object,cx:&Context)->Result<i64> {
 if let ObjectType::Int(n)=value.untag(){Ok(n)}else{Err(registration_type(value,"integerp",cx))}
}
fn registration_range(value:Object,min:i64,max:i64,cx:&Context)->Result<i64> {
 let n=registration_int(value,cx)?;
 if !(min..=max).contains(&n){return Err(crate::eval::EvalError::signal(crate::sym::ARGS_OUT_OF_RANGE.into(),list!(value,min,max;cx),cx).into())}Ok(n)
}

/// Bounded offset-method subset of the actual 17-argument native ABI.
/// Unsupported maps/subsets/supersets/unification fail BEFORE publication.
#[defun(name = "define-charset-internal")]
fn define_charset_internal_abi<'c>(
 name:Object<'c>,dimension:Object<'c>,space:Object<'c>,min:Object<'c>,max:Object<'c>,
 iso_final:Object<'c>,iso_revision:Object<'c>,mule_id:Object<'c>,ascii:Object<'c>,
 supplementary:Object<'c>,invalid:Object<'c>,offset:Object<'c>,map:Object<'c>,
 subset:Object<'c>,superset:Object<'c>,unify:Object<'c>,plist:Object<'c>,
 env:&mut Rt<Env>,cx:&'c Context,
)->Result<()> {
 // Fixed native arity gives the defun wrapper a bounded rooted argument array.
 // The implementation only borrows Context, so no moving collection occurs here.
 define_charset_internal(&[name,dimension,space,min,max,iso_final,iso_revision,
  mule_id,ascii,supplementary,invalid,offset,map,subset,superset,unify,plist],env,cx)
}

fn define_charset_internal<'c>(args:&[Object<'c>],env:&mut Rt<Env>,cx:&'c Context)->Result<()> {
 if args.len()!=17 {return Err(crate::eval::EvalError::signal(crate::sym::WRONG_NUMBER_OF_ARGUMENTS.into(),list!(intern("define-charset-internal",cx),args.len() as i64;cx),cx).into())}
 let ObjectType::Symbol(name)=args[0].untag()else{return Err(registration_type(args[0],"symbolp",cx))};
 let ObjectType::Vec(space)=args[2].untag()else{return Err(registration_type(args[2],"vectorp",cx))};
 // GNU's short/malformed geometry can enter unsafe C paths. Reject boundedly.
 anyhow::ensure!(space.len()==8,"Bounded charset geometry requires eight entries");
 let mut geometry=[[0u8;2];4];let mut inferred=0;
 for i in 0..4 {let lo=registration_range(space[2*i].get(),0,255,cx)? as u8;let hi=registration_range(space[2*i+1].get(),i64::from(lo),255,cx)? as u8;geometry[i]=[lo,hi];if hi>0{inferred=i+1;}}
 let dimension=if args[1].is_nil(){inferred as u8}else{registration_range(args[1],1,4,cx)? as u8};
 anyhow::ensure!(dimension>0 && usize::from(dimension)==inferred,"Degenerate or dimension-mismatched charset geometry is unsupported");
 let low=geometry.iter().enumerate().fold(0u32,|n,(i,b)|n|(u32::from(b[0])<<(8*i)));
 let high=geometry.iter().enumerate().fold(0u32,|n,(i,b)|n|(u32::from(b[1])<<(8*i)));
 let min=if args[3].is_nil(){low}else{registration_range(args[3],i64::from(low),i64::from(high),cx)? as u32};
 let max=if args[4].is_nil(){high}else{registration_range(args[4],i64::from(min),i64::from(high),cx)? as u32};
 let invalid=if args[10].is_nil(){if min>0{0}else{max.checked_add(1).ok_or_else(||anyhow::anyhow!("Attribute :invalid-code must be specified"))?}}else{registration_range(args[10],0,i64::from(u32::MAX),cx)? as u32};
 let iso_final=if args[5].is_nil(){None}else{Some(registration_range(args[5],48,127,cx)? as u8)};
 let iso_revision=if args[6].is_nil(){-1}else{registration_range(args[6],-1,63,cx)? as i8};
 let mule_id=if args[7].is_nil(){None}else{let n=registration_range(args[7],0,255,cx)? as u8;anyhow::ensure!(n==0||n>128,"Invalid emacs-mule-id: {n}");Some(n)};
 anyhow::ensure!(iso_final.is_none()||dimension<=3,"Unsupported ISO charset dimension");
 anyhow::ensure!(!args[11].is_nil(),"Only offset charset registration is implemented; maps and parent methods remain unsupported");
 let offset=registration_range(args[11],0,0x3fffff,cx)? as u32;
 let mut desc=OffsetDescriptor{space:geometry,min,max,offset,invalid,ascii:!args[8].is_nil()};
 let min_rank=desc.rank(min).ok_or_else(||anyhow::anyhow!("Minimum charset code is in a geometry hole"))?;
 let max_rank=desc.rank(max).ok_or_else(||anyhow::anyhow!("Maximum charset code is in a geometry hole"))?;
 let last=max_rank.checked_sub(min_rank).and_then(|n|n.checked_add(u64::from(offset))).ok_or_else(||anyhow::anyhow!("Charset offset overflow"))?;
 anyhow::ensure!(last<=0x3fffff,"Unsupported max char: {last}");
 if offset==0&&last>=128{desc.ascii=true;}
 anyhow::ensure!(args[15].is_nil(),"Charset unification is not implemented");
 if !args[16].is_nil()&&!matches!(args[16].untag(),ObjectType::Cons(_)){return Err(registration_type(args[16],"listp",cx))}
 anyhow::ensure!(env.charset_seeded,"Charset registry has not been initialized");
 let old=env.charset_registry.get(name).map(|v|v.bind(cx));
 let is_new=old.is_none();
 let id=if let Some(old)=old{let ObjectType::Vec(v)=old.untag()else{unreachable!()};let ObjectType::Int(n)=v[0].get().untag()else{unreachable!()};n as usize}else{env.charset_offsets.len()};
 anyhow::ensure!(!is_new||id<4096,"Charset ID limit exceeded");
 anyhow::ensure!(!is_new||env.charset_registry.iter().count()<CHARSET_REGISTRY_KEY_LIMIT,"Charset registry key limit exceeded");
 anyhow::ensure!(env.charset_registration.publications<65536,"Charset registration publication limit exceeded");
 let next_tick=env.charset_registration.tick.checked_add(u64::from(is_new)).ok_or_else(||anyhow::anyhow!("Charset priority tick overflow"))?;
 // All retained vectors are bounded by 4096 IDs, ISO by 768 and Mule by 256.
 // Reserve every potential growth before any semantic state becomes visible.
 env.charset_registry.try_reserve(usize::from(is_new))?;
 env.charset_offsets.try_reserve(usize::from(is_new))?;env.charset_names.try_reserve(usize::from(is_new))?;env.charset_attributes.try_reserve(usize::from(is_new))?;
 let state=&mut env.charset_registration;
 state.meta.try_reserve(usize::from(is_new))?;state.priority.try_reserve(usize::from(is_new))?;
 state.iso_members.try_reserve(usize::from(is_new&&iso_final.is_some()))?;
 state.mule_members.try_reserve(usize::from(is_new&&mule_id.is_some()))?;
 let list_symbol=intern("charset-list",cx);
 let list_missing=is_new&&env.vars.get(list_symbol).is_none();
 env.vars.try_reserve(usize::from(list_missing))?;
 let list:Option<Object>=if is_new{Some(Cons::new(name,env.vars.get(list_symbol).map_or(NIL,|s|s.bind(cx)),cx).into())}else{None};
 let new_attrs:Object=cx.add(vec![Object::from(id as i64),args[16]]).into();
 let meta=RegistrationMeta{dimension,iso_final,iso_revision,mule_id,supplementary:!args[9].is_nil()};
 // No callbacks, collection, recoverable errors, or container growth below.
 if is_new {
  let pos=if meta.supplementary{env.charset_registration.priority.len()}else{env.charset_registration.priority.iter().position(|&i|env.charset_registration.meta[i].supplementary).unwrap_or(env.charset_registration.priority.len())};
  env.charset_registration.priority.insert(pos,id);
  env.charset_offsets.push(desc);env.charset_names.push(name);env.charset_attributes.push(new_attrs);env.charset_registration.meta.push(meta);
 }else{env.charset_offsets[id]=desc;env.charset_names[id].set(name);env.charset_attributes[id].set(new_attrs);env.charset_registration.meta[id]=meta;}
 env.charset_registry.insert(name,new_attrs);
 if let Some(list)=list{env.vars.insert(list_symbol,list);}
 let state=&mut env.charset_registration;
 if let Some(final_char)=iso_final{state.iso[RegistrationState::iso_index(dimension,geometry[0][1]-geometry[0][0]==95,final_char)]=Some(id);if is_new{state.iso_members.push(id);}}
 if let Some(mule)=mule_id{state.mule[usize::from(mule)]=Some((id,dimension+if mule<160{1}else{2}));if is_new{state.mule_members.push(id);}}
 // Native redefinition leaves old designation slots pointing at the same ID.
 state.tick=next_tick;state.publications+=1;
 Ok(())
}
#[defun]
fn iso_charset<'c>(dimension:Object,chars:Object,final_char:Object,env:&Rt<Env>,cx:&'c Context)->Result<Object<'c>> {
 let dimension=registration_range(dimension,1,3,cx)? as u8;
 let chars=registration_int(chars,cx)?;anyhow::ensure!(chars==94||chars==96,"Invalid ISO CHARS");
 let final_char=registration_range(final_char,48,126,cx)? as u8;
 Ok(env.charset_registration.iso.get(RegistrationState::iso_index(dimension,chars==96,final_char)).copied().flatten().map_or(NIL,|id|env.charset_names[id].bind(cx).into()))
}
#[defun]
fn charset_priority_list<'c>(highest:Option<Object>,env:&Rt<Env>,cx:&'c Context)->Object<'c> {
 if highest.is_some_and(|h|!h.is_nil()){return env.charset_registration.priority.first().map_or(NIL,|&id|env.charset_names[id].bind(cx).into());}
 let mut result=NIL;for &id in env.charset_registration.priority.iter().rev(){result=Cons::new(env.charset_names[id].bind(cx),result,cx).into();}result
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

 #[test]
 fn registration_iso_mule_offset_and_priority_authority() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(define-charset-internal 'rune-latin 1 [32 127 0 0 0 0 0 0] nil nil 65 nil 129 nil nil nil 160 nil nil nil nil '(:phase first))",cx,env).unwrap();
  assert_eq!(iso_charset(1.into(),96.into(),65.into(),env,cx).unwrap().to_string(),"rune-latin");
  assert_eq!(decode_char(intern("rune-latin",cx).into(),127.into(),env,cx).unwrap().to_string(),"255");
  assert_eq!(charset_priority_list(None,env,cx).to_string(),"(ascii iso-8859-1 unicode rune-latin emacs eight-bit)");
  assert_eq!(env.charset_registration.mule[129],Some((5,2)));
  assert_eq!(env.charset_registration.iso_members,vec![0,5]);assert_eq!(env.charset_registration.mule_members,vec![0,5]);
  let meta=&env.charset_registration.meta[5];assert_eq!(meta.dimension,1);assert_eq!(meta.iso_final,Some(65));assert_eq!(meta.iso_revision,-1);assert_eq!(meta.mule_id,Some(129));
  crate::lread::load_internal("(set-charset-plist 'rune-latin '(:iso-final-char 70 :code-offset 0))",cx,env).unwrap();
  assert_eq!(decode_char(intern("rune-latin",cx).into(),32.into(),env,cx).unwrap().to_string(),"160");
  assert_eq!(iso_charset(1.into(),96.into(),65.into(),env,cx).unwrap().to_string(),"rune-latin");
 }
 #[test]
 fn registration_redefinition_alias_identity_and_old_designation() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(progn (define-charset-internal 'rune-first 1 [32 127 0 0 0 0 0 0] nil nil 65 nil 129 nil nil nil 160 nil nil nil nil '(:phase first)) (define-charset-alias 'rune-second 'rune-first))",cx,env).unwrap();
  let tick=env.charset_registration.tick;
  let old=charset_plist(intern("rune-first",cx).into(),env,cx).unwrap();root!(old,cx);
  crate::lread::load_internal("(define-charset-internal 'rune-second 1 [32 127 0 0 0 0 0 0] nil nil 70 1 130 nil t nil 512 nil nil nil nil '(:phase second))",cx,env).unwrap();
  assert_eq!(env.charset_registration.tick,tick);assert_eq!(env.charset_offsets.len(),6);
  assert!(charset_plist(intern("rune-first",cx).into(),env,cx).unwrap().ptr_eq(old.bind(cx)));
  assert_eq!(decode_char(intern("rune-first",cx).into(),32.into(),env,cx).unwrap().to_string(),"512");
  for final_char in [65,70]{assert_eq!(iso_charset(1.into(),96.into(),final_char.into(),env,cx).unwrap().to_string(),"rune-second");}
  assert_eq!(env.charset_registration.mule[129],Some((5,2)));assert_eq!(env.charset_registration.mule[130],Some((5,2)));
  assert_eq!(env.charset_registration.iso_members,vec![0,5]);
  assert_eq!(charset_priority_list(None,env,cx).to_string(),"(ascii iso-8859-1 unicode rune-second emacs eight-bit)");
 }
 #[test]
 fn registration_rejects_unsupported_and_capacity_without_publication() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  let list=env.vars.get(intern("charset-list",cx)).unwrap().bind(cx);root!(list,cx);
  for form in [
   "(define-charset-internal 'blocked 1 [32 35 0 0 0 0 0 0] nil nil nil nil nil nil nil nil nil [32 256] nil nil nil nil)",
   "(define-charset-internal 'blocked 1 [32 35 0 0 0 0 0 0] nil nil nil nil nil nil nil nil 256 nil nil nil [] nil)",
   "(define-charset-internal 'blocked 1 [32 35 0 0 0 0 0 0] nil nil nil nil nil nil nil nil 4194303 nil nil nil nil nil)",
   "(define-charset-internal 'blocked 4 [32 35 0 0 0 0 0 0] nil nil 65 nil nil nil nil nil 256 nil nil nil nil nil)",
   "(define-charset-internal 'blocked 1 [32 35 0 0 0 0 0 0] nil nil nil nil nil nil nil nil 256 nil nil nil nil 42)"] {
    assert!(crate::lread::load_internal(form,cx,env).is_err());assert_eq!(env.charset_offsets.len(),5);assert_eq!(env.charset_registration.tick,5);assert_eq!(env.charset_registry.iter().count(),5);assert!(env.vars.get(intern("charset-list",cx)).unwrap().bind(cx).ptr_eq(list.bind(cx)));
  }
  env.charset_registration.publications=65536;
  assert!(crate::lread::load_internal("(define-charset-internal 'ascii 1 [0 127 0 0 0 0 0 0] nil nil nil nil nil nil nil nil 0 nil nil nil nil nil)",cx,env).is_err());
  assert_eq!(decode_char(intern("ascii",cx).into(),65.into(),env,cx).unwrap().to_string(),"65");
 }
 #[test]
 fn registration_canonical_name_traces_after_alias_retarget() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  crate::lread::load_internal("(let ((name (make-symbol \"private-registration\")) (cycle (cons 42 nil))) (setcdr cycle cycle) (define-charset-internal name 1 [32 35 0 0 0 0 0 0] nil nil 65 nil 129 nil nil nil 256 nil nil nil nil cycle) (define-charset-alias name 'ascii) (setq charset-list nil))",cx,env).unwrap();
  for _ in 0..4 {env.initialize_condition_metadata(cx);cx.garbage_collect(true);
   let name=iso_charset(1.into(),94.into(),65.into(),env,cx).unwrap();assert_eq!(name.to_string(),"private-registration");assert_eq!(charset_id_internal(Some(name),env,cx).unwrap().to_string(),"0");assert!(Object::from(env.charset_names[5].bind(cx)).ptr_eq(name));
   let ObjectType::Vec(attrs)=env.charset_attributes[5].bind(cx).untag()else{panic!("canonical attributes lost")};
   let plist=attrs[1].get();let ObjectType::Cons(pair)=plist.untag()else{panic!("canonical cycle lost")};
   assert_eq!(pair.car().to_string(),"42");assert!(pair.cdr().ptr_eq(plist));
   assert_eq!(env.charset_offsets[5].decode(32),Some(256));
  }
  root!(other,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,other);assert!(iso_charset(1.into(),94.into(),65.into(),other,cx).unwrap().is_nil());
 }
 #[test]
 fn registration_offset_iso_and_redefinition_match_real_gnu() {
  let source="(let ((name (make-symbol \"rune-offset-oracle\")) (alias (make-symbol \"rune-alias-oracle\")) (first (list :phase 'first))) (define-charset-internal name 1 [32 127 0 0 0 0 0 0] nil nil 65 nil 129 nil nil nil 160 nil nil nil nil first) (define-charset-alias alias name) (let ((id (charset-id-internal name)) (before charset-list)) (define-charset-internal alias 1 [32 127 0 0 0 0 0 0] nil nil 70 1 130 nil t nil 512 nil nil nil nil nil) (list (= id (charset-id-internal alias)) (eq before charset-list) (eq (charset-plist name) first) (decode-char name 32) (encode-char 607 alias) (eq (iso-charset 1 96 65) alias) (eq (iso-charset 1 96 70) alias))))";
  crate::library::elprop::start_emacs().unwrap();let native=crate::library::elprop::eval(source).unwrap();
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  let form=crate::reader::read(source,cx).unwrap().0;root!(form,cx);let actual=crate::interpreter::eval(form,None,env,cx).unwrap().to_string();assert_eq!(actual,native);
 }
 #[test]
 fn registration_id_capacity_redefinition_and_rejection_are_atomic() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  // Directly construct the bounded N-1 fixture to isolate boundary admission;
  // the final registration and replacement run through the public primitive.
  for id in 5..4095 {
   let name=crate::core::object::Symbol::new_uninterned("capacity-registration",cx);
   let attributes:Object=cx.add(vec![Object::from(id as i64),NIL]).into();
   env.charset_registry.insert(name,attributes);env.charset_names.push(name);env.charset_attributes.push(attributes);
   env.charset_offsets.push(seed_descriptor(0));
   env.charset_registration.meta.push(RegistrationMeta{dimension:1,iso_final:None,iso_revision:-1,mule_id:None,supplementary:false});
   env.charset_registration.priority.push(id);
  }
  crate::lread::load_internal("(define-charset-internal 'capacity-last 1 [32 35 0 0 0 0 0 0] nil nil 65 nil 129 nil nil nil 256 nil nil nil nil nil)",cx,env).unwrap();
  assert_eq!(env.charset_offsets.len(),4096);assert_eq!(env.charset_registry.iter().count(),4096);
  let tick=env.charset_registration.tick;
  let list=env.vars.get(intern("charset-list",cx)).unwrap().bind(cx);root!(list,cx);
  let priority=env.charset_registration.priority.clone();let iso=env.charset_registration.iso.clone();let mule=env.charset_registration.mule.clone();
  assert!(crate::lread::load_internal("(define-charset-internal 'capacity-overflow 1 [32 35 0 0 0 0 0 0] nil nil 70 nil 130 nil nil nil 512 nil nil nil nil nil)",cx,env).is_err());
  assert!(!charsetp(intern("capacity-overflow",cx).into(),env));assert_eq!(env.charset_offsets.len(),4096);assert_eq!(env.charset_registration.tick,tick);
  assert_eq!(env.charset_registration.priority,priority);assert_eq!(env.charset_registration.iso,iso);assert_eq!(env.charset_registration.mule,mule);
  assert!(env.vars.get(intern("charset-list",cx)).unwrap().bind(cx).ptr_eq(list.bind(cx)));
  crate::lread::load_internal("(define-charset-internal 'capacity-last 1 [32 35 0 0 0 0 0 0] nil nil 70 nil 130 nil nil nil 512 nil nil nil nil nil)",cx,env).unwrap();
  assert_eq!(env.charset_offsets.len(),4096);assert_eq!(env.charset_registry.iter().count(),4096);assert_eq!(env.charset_registration.tick,tick);assert_eq!(env.charset_registration.priority,priority);
  assert_eq!(decode_char(intern("capacity-last",cx).into(),32.into(),env,cx).unwrap().to_string(),"512");
  assert_eq!(charset_id_internal(Some(intern("capacity-last",cx).into()),env,cx).unwrap().to_string(),"4095");
  assert!(env.vars.get(intern("charset-list",cx)).unwrap().bind(cx).ptr_eq(list.bind(cx)));
 }
 #[test]
 fn registration_alias_key_capacity_does_not_consume_descriptor_id() {
  let roots=RootSet::default();let cx=&mut Context::new(&roots);crate::sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);crate::core::env::init_variables(cx,env);
  let ascii=attrs(intern("ascii",cx).into(),env,cx).unwrap();
  for _ in 5..4096 {env.charset_registry.insert(crate::core::object::Symbol::new_uninterned("capacity-alias",cx),ascii);}
  let list=env.vars.get(intern("charset-list",cx)).unwrap().bind(cx);root!(list,cx);
  assert!(crate::lread::load_internal("(define-charset-internal 'capacity-new 1 [32 35 0 0 0 0 0 0] nil nil nil nil nil nil nil nil 256 nil nil nil nil nil)",cx,env).is_err());
  assert_eq!(env.charset_offsets.len(),5);assert_eq!(env.charset_registration.tick,5);assert_eq!(env.charset_registration.publications,0);
  assert!(env.vars.get(intern("charset-list",cx)).unwrap().bind(cx).ptr_eq(list.bind(cx)));
  crate::lread::load_internal("(define-charset-internal 'ascii 1 [0 127 0 0 0 0 0 0] nil nil nil nil nil nil nil nil 256 nil nil nil nil nil)",cx,env).unwrap();
  assert_eq!(env.charset_offsets.len(),5);assert_eq!(env.charset_registry.iter().count(),4096);assert_eq!(env.charset_registration.tick,5);
  assert_eq!(decode_char(intern("ascii",cx).into(),65.into(),env,cx).unwrap().to_string(),"321");
 }
}
