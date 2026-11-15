//! Experimental native admission kernel only. No Lisp execution, heap access,
//! function publication, scheduler, or channel behavior is enabled by this file.
use std::{collections::{HashMap,VecDeque},marker::PhantomData,rc::Rc,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}}};
static NEXT_RUNTIME:AtomicU64=AtomicU64::new(1);
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)] pub struct RuntimeId(u64);
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)] pub struct JobId {runtime:RuntimeId,sequence:u64}
#[derive(Clone,Copy,Debug,PartialEq,Eq)] pub enum Error {Closed,Foreign,Unknown,WrongState,Capacity,Exhausted,Poisoned}
#[derive(Clone,Copy,Debug,PartialEq,Eq)] enum Phase {Reserved,Ready,Running{cancel:bool},Completed,Cancelled}
struct State {runtime:RuntimeId,open:bool,limit:usize,next:u64,jobs:HashMap<JobId,Phase>,ready:VecDeque<JobId>}
/// Native IDs/intents only. It cannot read or own a Lisp payload.
#[derive(Clone)] pub struct Sender(Arc<Mutex<State>>);
/// Owner control is intentionally !Send/!Sync. No mutex guard escapes an API.
pub struct Owner {state:Arc<Mutex<State>>,_owner:PhantomData<Rc<()>>}
impl Owner {
 pub fn new(limit:usize)->Result<(Self,Sender),Error>{
  if limit==0 || limit>4096 {return Err(Error::Capacity)}
  let id=NEXT_RUNTIME.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|n|n.checked_add(1)).map_err(|_|Error::Exhausted)?;
  let mut jobs=HashMap::new();jobs.try_reserve(limit).map_err(|_|Error::Capacity)?;
  let mut ready=VecDeque::new();ready.try_reserve(limit).map_err(|_|Error::Capacity)?;
  let state=Arc::new(Mutex::new(State{runtime:RuntimeId(id),open:true,limit,next:1,jobs,ready}));
  Ok((Self{state:state.clone(),_owner:PhantomData},Sender(state)))
 }
 /// Reserve BEFORE staging owner-rooted payload. Capacity includes terminal
 /// records until owner reaping, so native cancellation never drops Lisp roots.
 pub fn reserve(&mut self)->Result<JobId,Error>{
  let mut s=self.state.lock().map_err(|_|Error::Poisoned)?;
  if !s.open{return Err(Error::Closed)} if s.jobs.len()>=s.limit{return Err(Error::Capacity)}
  let next=s.next.checked_add(1).ok_or(Error::Exhausted)?;
  let id=JobId{runtime:s.runtime,sequence:s.next};s.next=next;s.jobs.insert(id,Phase::Reserved);Ok(id)
 }
 /// Owner commits Ready only AFTER its separate payload/root registry is ready.
 /// Failure leaves that registry responsible for dropping the staged root.
 pub fn commit(&mut self,id:JobId)->Result<(),Error>{
  let mut s=self.state.lock().map_err(|_|Error::Poisoned)?;check(&s,id)?;
  if s.jobs.get(&id)!=Some(&Phase::Reserved){return Err(Error::WrongState)}
  s.ready.push_back(id);s.jobs.insert(id,Phase::Ready);Ok(())
 }
 /// Returned ID is not an execution capability for any heap. Caller must bind
 /// the separate owner payload registry; execution happens AFTER this lock ends.
 pub fn take_next(&mut self)->Result<Option<JobId>,Error>{
  let mut s=self.state.lock().map_err(|_|Error::Poisoned)?;if !s.open{return Err(Error::Closed)}
  while let Some(id)=s.ready.pop_front(){if s.jobs.get(&id)==Some(&Phase::Ready){s.jobs.insert(id,Phase::Running{cancel:false});return Ok(Some(id))}}
  Ok(None)
 }
 pub fn cancellation_requested(&self,id:JobId)->Result<bool,Error>{
  let s=self.state.lock().map_err(|_|Error::Poisoned)?;check_known(&s,id)?;
  match s.jobs.get(&id){Some(Phase::Running{cancel})=>Ok(*cancel),_=>Err(Error::WrongState)}
 }
 pub fn finish(&mut self,id:JobId)->Result<(),Error>{self.finish_inner(id,false)}
 /// Native state only: caller may use this AFTER a job has unwound. This does
 /// not catch Rust panics or recover a partially corrupted Lisp interpreter.
 pub fn abandon(&mut self,id:JobId)->Result<(),Error>{self.finish_inner(id,true)}
 fn finish_inner(&mut self,id:JobId,abandoned:bool)->Result<(),Error>{
  let mut s=self.state.lock().map_err(|_|Error::Poisoned)?;check_known(&s,id)?;
  let phase=match s.jobs.get(&id){Some(Phase::Running{cancel})=>if *cancel||abandoned{Phase::Cancelled}else{Phase::Completed},_=>return Err(Error::WrongState)};
  s.jobs.insert(id,phase);Ok(())
 }
 /// Remove terminal IDs only. A future owner root registry must drop matching
 /// payloads after this method returns, never under the native mutex.
 pub fn reap(&mut self)->Result<Vec<JobId>,Error>{
  let mut s=self.state.lock().map_err(|_|Error::Poisoned)?;
  let ids:Vec<_>=s.jobs.iter().filter_map(|(id,p)|matches!(p,Phase::Completed|Phase::Cancelled).then_some(*id)).collect();
  for id in &ids{s.jobs.remove(id);}
  // Reaping must also remove cancelled queue tombstones. Otherwise repeated
  // commit/cancel/reap without dequeue would grow the FIFO without bound.
  let State{jobs,ready,..}=&mut *s;ready.retain(|id|jobs.get(id)==Some(&Phase::Ready));
  Ok(ids)
 }
 /// Closes admission and cancels IDs; does NOT interrupt a running interpreter
 /// or prove root teardown safe. Real integration must unwind execution first.
 pub fn close(&mut self)->Result<(),Error>{
  let mut s=self.state.lock().map_err(|_|Error::Poisoned)?;s.open=false;s.ready.clear();
  for p in s.jobs.values_mut(){match p{Phase::Running{cancel}=>*cancel=true,Phase::Reserved|Phase::Ready=>*p=Phase::Cancelled,_=>{}}}Ok(())
 }
}
fn check(s:&State,id:JobId)->Result<(),Error>{if !s.open{return Err(Error::Closed)}check_known(s,id)}
fn check_known(s:&State,id:JobId)->Result<(),Error>{if id.runtime!=s.runtime{return Err(Error::Foreign)}if !s.jobs.contains_key(&id){return Err(Error::Unknown)}Ok(())}
impl Sender {
 pub fn cancel(&self,id:JobId)->Result<(),Error>{
  let mut s=self.0.lock().map_err(|_|Error::Poisoned)?;check(&s,id)?;
  match s.jobs.get_mut(&id).unwrap(){Phase::Reserved|Phase::Ready=>{*s.jobs.get_mut(&id).unwrap()=Phase::Cancelled;Ok(())},Phase::Running{cancel}=>{*cancel=true;Ok(())},_=>Err(Error::WrongState)}
 }
}
impl Drop for Owner {fn drop(&mut self){if let Ok(mut s)=self.state.lock(){s.open=false;s.ready.clear();s.jobs.clear();}}}
#[cfg(test)] mod tests {
 use super::*;
 #[test]fn fifo_capacity_and_owner_reaping(){let(mut o,_)=Owner::new(2).unwrap();let a=o.reserve().unwrap();let b=o.reserve().unwrap();assert_eq!(o.reserve(),Err(Error::Capacity));o.commit(a).unwrap();o.commit(b).unwrap();assert_eq!(o.take_next(),Ok(Some(a)));o.finish(a).unwrap();assert_eq!(o.reserve(),Err(Error::Capacity));assert_eq!(o.reap().unwrap(),vec![a]);assert_eq!(o.take_next(),Ok(Some(b)));o.finish(b).unwrap();}
 #[test]fn cancelled_reservation_never_runs(){let(mut o,s)=Owner::new(1).unwrap();let a=o.reserve().unwrap();s.cancel(a).unwrap();assert_eq!(o.commit(a),Err(Error::WrongState));assert_eq!(o.take_next(),Ok(None));assert_eq!(o.reap().unwrap(),vec![a]);}
 #[test]fn cross_thread_cancel_while_owner_runs_holds_no_mutex(){let(mut o,s)=Owner::new(1).unwrap();let a=o.reserve().unwrap();o.commit(a).unwrap();assert_eq!(o.take_next(),Ok(Some(a)));std::thread::spawn(move||s.cancel(a).unwrap()).join().unwrap();assert!(o.cancellation_requested(a).unwrap());o.finish(a).unwrap();assert_eq!(o.reap().unwrap(),vec![a]);}
 #[test]fn stale_and_foreign_ids_fail(){let(mut a,sa)=Owner::new(1).unwrap();let(mut b,sb)=Owner::new(1).unwrap();let id=a.reserve().unwrap();assert_eq!(sb.cancel(id),Err(Error::Foreign));assert_eq!(b.commit(id),Err(Error::Foreign));sa.cancel(id).unwrap();a.reap().unwrap();assert_eq!(sa.cancel(id),Err(Error::Unknown));let fresh=a.reserve().unwrap();assert_ne!(id,fresh);}
 #[test]fn owner_drop_closes_escaped_sender(){let(mut o,s)=Owner::new(1).unwrap();let a=o.reserve().unwrap();drop(o);assert_eq!(s.cancel(a),Err(Error::Closed));}
 #[test]fn cancellation_of_ready_job_preserves_fifo_successor(){let(mut o,s)=Owner::new(2).unwrap();let a=o.reserve().unwrap();let b=o.reserve().unwrap();o.commit(a).unwrap();o.commit(b).unwrap();s.cancel(a).unwrap();assert_eq!(o.take_next(),Ok(Some(b)));assert_eq!(o.reap().unwrap(),vec![a]);}
 #[test]fn sequence_exhaustion_is_no_publication(){let(mut o,_)=Owner::new(2).unwrap();o.state.lock().unwrap().next=u64::MAX;assert_eq!(o.reserve(),Err(Error::Exhausted));assert!(o.state.lock().unwrap().jobs.is_empty());}
 #[test]fn sender_is_send_sync_without_payload(){fn check<T:Send+Sync>(){}check::<Sender>();}
 #[test]fn close_keeps_running_until_explicit_owner_completion(){let(mut o,s)=Owner::new(2).unwrap();let a=o.reserve().unwrap();o.commit(a).unwrap();o.take_next().unwrap();let b=o.reserve().unwrap();o.commit(b).unwrap();o.close().unwrap();assert!(o.cancellation_requested(a).unwrap());assert_eq!(s.cancel(a),Err(Error::Closed));assert_eq!(o.reap().unwrap(),vec![b]);o.finish(a).unwrap();assert_eq!(o.reap().unwrap(),vec![a]);}
 #[test]fn cancel_dequeue_race_has_one_consistent_running_or_cancelled_state(){for _ in 0..32{let(mut o,s)=Owner::new(1).unwrap();let id=o.reserve().unwrap();o.commit(id).unwrap();let gate=Arc::new(std::sync::Barrier::new(2));let g=gate.clone();let worker=std::thread::spawn(move||{g.wait();s.cancel(id).unwrap();});gate.wait();let chosen=o.take_next().unwrap();worker.join().unwrap();if let Some(job)=chosen{assert_eq!(job,id);assert!(o.cancellation_requested(id).unwrap());assert!(o.reap().unwrap().is_empty());o.finish(id).unwrap();}assert_eq!(o.reap().unwrap(),vec![id]);assert_eq!(o.take_next(),Ok(None));}}
 #[test]fn owner_reentry_and_native_panic_abandon_do_not_hold_admission_lock(){let(mut o,s)=Owner::new(2).unwrap();let id=o.reserve().unwrap();o.commit(id).unwrap();o.take_next().unwrap();let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{let nested=o.reserve().unwrap();o.commit(nested).unwrap();s.cancel(nested).unwrap();panic!("native fixture only");}));assert!(outcome.is_err());o.abandon(id).unwrap();assert_eq!(o.reap().unwrap().len(),2);}
 #[test]fn escaped_native_sender_creates_no_arc_cycle(){let(mut o,s)=Owner::new(1).unwrap();o.reserve().unwrap();let weak=Arc::downgrade(&o.state);drop(o);assert!(weak.upgrade().is_some());drop(s);assert!(weak.upgrade().is_none());}
 #[test]fn repeated_cancel_reap_without_dequeue_keeps_native_fifo_bounded(){let(mut o,s)=Owner::new(1).unwrap();for _ in 0..1024{let id=o.reserve().unwrap();o.commit(id).unwrap();s.cancel(id).unwrap();assert_eq!(o.reap().unwrap(),vec![id]);let state=o.state.lock().unwrap();assert!(state.ready.is_empty());assert!(state.jobs.is_empty());}}
 #[test]fn ready_fifo_is_commit_order_not_reservation_order(){let(mut owner,_)=Owner::new(2).unwrap();let a=owner.reserve().unwrap();let b=owner.reserve().unwrap();owner.commit(b).unwrap();owner.commit(a).unwrap();assert_eq!(owner.take_next(),Ok(Some(b)));owner.finish(b).unwrap();assert_eq!(owner.take_next(),Ok(Some(a)));owner.finish(a).unwrap();}

}
