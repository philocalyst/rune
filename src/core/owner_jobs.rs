//! Dormant owner-local rooted job foundation. No production driver imports or
//! enables it. IDs/cancellation cross threads; payload roots never do.
mod admission;
pub(crate) use admission::{JobId,Sender};
use crate::core::{gc::{Context,OwnerLiveness},object::{HeapDomainValue,Object}};
use anyhow::{Result,ensure};
use std::{cell::{Cell,RefCell},collections::HashMap,rc::Rc,sync::Arc};
fn native<T>(r:std::result::Result<T,admission::Error>)->Result<T>{r.map_err(|e|anyhow::anyhow!("Owner job admission: {e:?}"))}
struct State {
 domain:u64,liveness:OwnerLiveness,closed:Cell<bool>,
 admission:RefCell<admission::Owner>,sender:Sender,
 queued:RefCell<HashMap<JobId,Arc<HeapDomainValue>>>,
}
impl State {
 fn validate_domain(&self,domain:u64)->Result<()>{ensure!(self.liveness.is_live()&&self.domain==domain,"Owner job heap has retired or belongs to another runtime");Ok(())}
 fn validate_owner(&self,cx:&Context)->Result<()>{self.validate_domain(cx.domain_id())}
 fn validate_open(&self,cx:&Context)->Result<()>{self.validate_owner(cx)?;ensure!(!self.closed.get(),"Owner job registry is closed");Ok(())}
 fn reap(&self)->Result<Vec<JobId>>{
  let ids=native(self.admission.borrow_mut().reap())?;
  let roots={let mut queued=self.queued.borrow_mut();ids.iter().filter_map(|id|queued.remove(id)).collect::<Vec<_>>()};
  // No queue/admission/RefCell guard remains while roots retire.
  drop(roots);Ok(ids)
 }
 fn close(&self)->Result<()> {
  self.closed.set(true);
  let result=native(self.admission.borrow_mut().close());
  // Queued work cannot execute after closure. Running roots are NOT here: each
  // RunningLease independently owns its payload and Rc<State> until unwind.
  let queued=std::mem::take(&mut *self.queued.borrow_mut());drop(queued);
  result?;self.reap()?;Ok(())
 }
}
/// Single owner control, !Send/!Sync through Rc. Dropping it never destroys a
/// RunningLease or its payload; all executing frames must remain scoped roots.
pub(crate) struct JobRegistry {state:Rc<State>}
impl JobRegistry {
 pub(crate) fn new(cx:&Context,limit:usize)->Result<Self>{
  let(admission,sender)=native(admission::Owner::new(limit))?;
  Ok(Self{state:Rc::new(State{domain:cx.domain_id(),liveness:cx.owner_liveness(),closed:Cell::new(false),admission:RefCell::new(admission),sender,queued:RefCell::new(HashMap::new())})})
 }
 pub(crate) fn sender(&self)->Sender{self.state.sender.clone()}
 /// Every reachable local allocation in value MUST belong to cx, and globals
 /// must satisfy the existing shared-graph contract. A matching Context stamp
 /// alone does not authenticate payload provenance. Caller must exclude GC and
 /// callbacks until this function has registered the root.
 pub(crate) unsafe fn enqueue_owned(&self,value:Object,cx:&Context)->Result<JobId>{unsafe{self.enqueue_inner(value,cx,0)}}
 unsafe fn enqueue_inner(&self,value:Object,cx:&Context,_cancel_stage:u8)->Result<JobId>{
  self.state.validate_open(cx)?;
  self.state.reap()?;
  self.state.queued.borrow_mut().try_reserve(1)?;
  let id=native(self.state.admission.borrow_mut().reserve())?;
  let mut reservation=Reservation{state:self.state.clone(),id,committed:false};
  #[cfg(test)] if _cancel_stage==1 {native(self.state.sender.cancel(id))?;}
  let root=Arc::new(unsafe{HeapDomainValue::new(value,cx)});
  self.state.queued.borrow_mut().insert(id,root);
  #[cfg(test)] if _cancel_stage==2 {native(self.state.sender.cancel(id))?;}
  native(self.state.admission.borrow_mut().commit(id))?;
  reservation.committed=true;Ok(id)
 }
 pub(crate) fn take_next(&self,cx:&Context)->Result<Option<RunningLease>>{
  self.state.validate_open(cx)?;self.state.reap()?;
  let Some(id)=native(self.state.admission.borrow_mut().take_next())? else{return Ok(None)};
  let payload=self.state.queued.borrow_mut().remove(&id).expect("private ready job has registered payload");
  Ok(Some(RunningLease{state:self.state.clone(),id,payload,active:Cell::new(false),finished:Cell::new(false)}))
 }
 pub(crate) fn reap_cancelled(&self,cx:&Context)->Result<Vec<JobId>>{self.state.validate_owner(cx)?;self.state.reap()}
 pub(crate) fn close(&self)->Result<()>{self.state.close()}
}
impl Drop for JobRegistry{fn drop(&mut self){let _=self.state.close();}}
struct Reservation {state:Rc<State>,id:JobId,committed:bool}
impl Drop for Reservation {fn drop(&mut self){if !self.committed{let _=self.state.sender.cancel(self.id);let _=self.state.reap();}}}
/// Owns one canonical payload root independently of the registry. Rc state keeps
/// native Running bookkeeping alive after JobRegistry drop; no state back-edge
/// owns this lease. It cannot cross threads or form a registry/lease Arc cycle.
pub(crate) struct RunningLease {state:Rc<State>,id:JobId,payload:Arc<HeapDomainValue>,active:Cell<bool>,finished:Cell<bool>}
impl RunningLease {
 pub(crate) fn id(&self)->JobId{self.id}
 pub(crate) fn enter(&self,cx:&Context)->Result<RunScope<'_>>{
  self.state.validate_open(cx)?;
  ensure!(!native(self.state.admission.borrow().cancellation_requested(self.id))?,"Owner job was cancelled");
  ensure!(!self.active.replace(true),"Owner job already has an active execution scope");
  Ok(RunScope{lease:self})
 }
 pub(crate) fn cancellation_requested(&self)->Result<bool>{native(self.state.admission.borrow().cancellation_requested(self.id))}
 pub(crate) fn finish(self)->Result<()>{
  ensure!(!self.active.get(),"Cannot retire an active owner job");
  native(self.state.admission.borrow_mut().finish(self.id))?;self.finished.set(true);self.state.reap()?;Ok(())
 }
}
impl Drop for RunningLease {fn drop(&mut self){
 // A RunScope borrows the lease, so safe Rust cannot destroy it while scoped
 // execution is active. No assertion claims recovery of a corrupted interpreter.
 if !self.finished.get(){let _=self.state.admission.borrow_mut().abandon(self.id);let _=self.state.reap();}
}}
/// This scope is not an evaluator. A real driver must put the job Env and all
/// active frame values in ordinary scoped GC roots before any callback/GC, then
/// unwind them BEFORE releasing this scope/lease. No registry guard escapes.
pub(crate) struct RunScope<'a>{lease:&'a RunningLease}
impl RunScope<'_>{
 pub(crate) fn bind<'cx>(&self,cx:&'cx Context)->Result<Object<'cx>>{self.lease.state.validate_owner(cx)?;self.lease.payload.bind(cx)}
}
impl Drop for RunScope<'_>{fn drop(&mut self){self.lease.active.set(false);}}

#[cfg(test)] mod tests {
 use super::*;use crate::core::{gc::RootSet,cons::Cons,object::{NIL,ObjectType}};
 #[test]fn queued_and_running_roots_follow_moving_collection(){let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,2).unwrap();let id=unsafe{registry.enqueue_owned(Cons::new(cx.add(7),NIL,cx).into(),cx)}.unwrap();cx.garbage_collect(true);let lease=registry.take_next(cx).unwrap().unwrap();assert_eq!(lease.id(),id);let scope=lease.enter(cx).unwrap();cx.garbage_collect(true);let ObjectType::Cons(c)=scope.bind(cx).unwrap().untag() else{unreachable!()};assert_eq!(c.car(),cx.add(7));drop(scope);lease.finish().unwrap();}
 #[test]fn registry_shutdown_keeps_active_payload_until_unwind(){let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,1).unwrap();unsafe{registry.enqueue_owned(cx.add("retained"),cx)}.unwrap();let lease=registry.take_next(cx).unwrap().unwrap();let scope=lease.enter(cx).unwrap();drop(registry);assert!(lease.cancellation_requested().unwrap());cx.garbage_collect(true);assert_eq!(scope.bind(cx).unwrap(),cx.add("retained"));drop(scope);lease.finish().unwrap();}
 #[test]fn escaped_lease_cannot_bind_retired_heap(){let roots=RootSet::default();let lease={let cx=Context::new(&roots);let registry=JobRegistry::new(&cx,1).unwrap();unsafe{registry.enqueue_owned(cx.add("retired"),&cx)}.unwrap();registry.take_next(&cx).unwrap().unwrap()};let roots2=RootSet::default();let cx=Context::new(&roots2);assert!(lease.enter(&cx).is_err());drop(lease);}
 #[test]fn cancellation_retires_queued_native_payload_once(){use crate::{channel_manager::{ChannelManager,RecvError},core::object::make_channel_pair_for_test};let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,1).unwrap();let manager=ChannelManager::isolated_for_test();let(id,sender,receiver)=make_channel_pair_for_test(manager.clone(),1);let job=unsafe{registry.enqueue_owned(cx.add(sender),cx)}.unwrap();registry.sender().cancel(job).unwrap();assert_eq!(registry.reap_cancelled(cx).unwrap(),vec![job]);cx.garbage_collect(true);assert!(matches!(receiver.try_recv(cx),Err(RecvError::Closed)));cx.garbage_collect(true);assert_eq!(manager.counts_for_test(id),Some((0,1)));receiver.close();}
 #[test]fn native_unwind_releases_scope_then_abandons_without_root_cycle(){let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,1).unwrap();unsafe{registry.enqueue_owned(cx.add("panic fixture"),cx)}.unwrap();let lease=registry.take_next(cx).unwrap().unwrap();let state=Rc::downgrade(&registry.state);let sender=registry.sender();let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{let _scope=lease.enter(cx).unwrap();cx.garbage_collect(true);panic!("native fixture, not interpreter recovery");}));assert!(outcome.is_err());assert!(!lease.active.get());drop(registry);drop(lease);assert!(state.upgrade().is_none());drop(sender);}
 #[test]fn ready_scope_holds_no_registry_borrow_across_owner_reentry(){let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,2).unwrap();unsafe{registry.enqueue_owned(cx.add(1),cx)}.unwrap();let lease=registry.take_next(cx).unwrap().unwrap();let scope=lease.enter(cx).unwrap();let nested=unsafe{registry.enqueue_owned(cx.add(2),cx)}.unwrap();let sender=registry.sender();std::thread::spawn(move||sender.cancel(nested).unwrap()).join().unwrap();registry.reap_cancelled(cx).unwrap();cx.garbage_collect(true);assert_eq!(scope.bind(cx).unwrap(),cx.add(1));drop(scope);lease.finish().unwrap();}
 #[test]fn staged_cancel_rolls_back_reserved_and_registered_roots(){
  use crate::{channel_manager::{ChannelManager,RecvError},core::object::make_channel_pair_for_test};
  for stage in [1,2]{let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,1).unwrap();let manager=ChannelManager::isolated_for_test();let(_,sender,receiver)=make_channel_pair_for_test(manager.clone(),1);let payload=cx.add(sender);
   // SAFETY: payload is allocated in this exact owner; hook only cancels a
   // native ID and cannot collect, evaluate or expose the uncommitted payload.
   assert!(unsafe{registry.enqueue_inner(payload,cx,stage)}.is_err());
   assert!(registry.state.queued.borrow().is_empty());assert!(registry.take_next(cx).unwrap().is_none());
   cx.garbage_collect(true);assert!(matches!(receiver.try_recv(cx),Err(RecvError::Closed)));receiver.close();
   let next=unsafe{registry.enqueue_owned(NIL,cx)}.unwrap();registry.sender().cancel(next).unwrap();registry.reap_cancelled(cx).unwrap();
  }
 }
 #[test]fn duplicate_enter_does_not_release_original_active_scope(){let roots=RootSet::default();let cx=&mut Context::new(&roots);let registry=JobRegistry::new(cx,1).unwrap();unsafe{registry.enqueue_owned(cx.add(7),cx)}.unwrap();let lease=registry.take_next(cx).unwrap().unwrap();let scope=lease.enter(cx).unwrap();assert!(lease.enter(cx).is_err());assert!(lease.active.get());cx.garbage_collect(true);assert_eq!(scope.bind(cx).unwrap(),cx.add(7));drop(scope);assert!(!lease.active.get());lease.finish().unwrap();}
 #[test]fn foreign_live_domain_gate_rejects_before_any_root_binding(){
  let roots=RootSet::default();let cx=Context::new(&roots);let registry=JobRegistry::new(&cx,1).unwrap();
  let(tx,rx)=std::sync::mpsc::channel();let gate=std::sync::Arc::new(std::sync::Barrier::new(2));let child_gate=gate.clone();
  let child=std::thread::spawn(move||{let roots=RootSet::default();let other=Context::new(&roots);tx.send((other.domain_id(),other.owner_liveness())).unwrap();child_gate.wait();});
  let(domain,witness)=rx.recv().unwrap();assert!(witness.is_live());assert!(registry.state.liveness.is_live());assert!(registry.state.validate_domain(domain).is_err());registry.state.validate_owner(&cx).unwrap();gate.wait();child.join().unwrap();
 }

}
