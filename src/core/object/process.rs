//! Genuine process objects contain native transport only. Lisp callbacks stay
//! in the owning Context registry and never enter reader/supervisor threads.
use super::{CloneIn, Gc, HeapDomainValue, IntoObject, NIL, Object};
use crate::{
    core::gc::{Block, Context, GcHeap, GcState, Trace},
    process_transport::{ProcessExit, ProcessTransport},
};
use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT_PROCESS: AtomicU64 = AtomicU64::new(1);
struct ProcessData {
    identity: u64,
    owner: u64,
    name: String,
    transport: Arc<ProcessTransport>,
}
pub(crate) struct ProcessInner {
    data: Arc<ProcessData>,
}
pub(crate) struct LispProcess(pub(in crate::core) GcHeap<ProcessInner>);
impl LispProcess {
    fn new(name: String, transport: Arc<ProcessTransport>, cx: &Context) -> Self {
        let identity = NEXT_PROCESS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("Process identities exhausted");
        Self(GcHeap::new(
            ProcessInner {
                data: Arc::new(ProcessData { identity, owner: cx.domain_id(), name, transport }),
            },
            false,
        ))
    }
    pub(crate) fn identity(&self) -> u64 {
        self.0.data.identity
    }
    pub(crate) fn name(&self) -> &str {
        &self.0.data.name
    }
    fn check(&self, cx: &Context) -> Result<()> {
        ensure!(self.0.data.owner == cx.domain_id(), "Process belongs to a different heap owner");
        Ok(())
    }
    pub(crate) fn transport(&self, cx: &Context) -> Result<Arc<ProcessTransport>> {
        self.check(cx)?;
        Ok(self.0.data.transport.clone())
    }
    pub(crate) fn callback_snapshot(&self, cx: &Context) -> Result<ProcessCallbackSnapshot> {
        cx.process_callback_snapshot(self)
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            crate::core::gc::AllocState::Forwarded(p) => Some(p),
            crate::core::gc::AllocState::Unmoved => None,
            crate::core::gc::AllocState::Global => panic!("global process in local finalizers"),
        }
    }
}
impl Trace for ProcessInner {
    fn trace(&self, _: &mut GcState) {}
}
impl<'new> CloneIn<'new, &'new Self> for LispProcess {
    fn clone_in<const C: bool>(&self, block: &'new Block<C>) -> anyhow::Result<Gc<&'new Self>> {
        Ok(Self(GcHeap::new(ProcessInner { data: self.0.data.clone() }, C)).into_obj(block))
    }
}
impl PartialEq for LispProcess {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}
impl Eq for LispProcess {}
impl fmt::Debug for LispProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<process:{}>", self.identity())
    }
}
impl fmt::Display for LispProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
crate::derive_GcMoveable!(LispProcess);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessCoding {
    Binary,
    Utf8,
    Utf8Unix,
}
#[derive(Clone, Debug)]
pub(crate) struct ProcessOwnerState {
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub pending_exit: Option<ProcessExit>,
    pub sentinel_delivered: bool,
    pub read_coding: ProcessCoding,
    pub write_coding: ProcessCoding,
    pub query_on_exit: bool,
    pub pending_utf8: Vec<u8>,
    pub stderr_pending_utf8: Vec<u8>,
}
impl Default for ProcessOwnerState {
    fn default() -> Self {
        Self {
            stdout_eof: false,
            stderr_eof: false,
            pending_exit: None,
            sentinel_delivered: false,
            read_coding: ProcessCoding::Binary,
            write_coding: ProcessCoding::Binary,
            query_on_exit: true,
            pending_utf8: Vec::new(),
            stderr_pending_utf8: Vec::new(),
        }
    }
}
#[derive(Clone)]
pub(crate) struct ProcessCallbackSnapshot {
    _owner_only: std::marker::PhantomData<std::rc::Rc<()>>,
    pub process: Arc<HeapDomainValue>,
    pub filter: Arc<HeapDomainValue>,
    pub sentinel: Arc<HeapDomainValue>,
    pub buffer: Arc<HeapDomainValue>,
    pub plist: Arc<HeapDomainValue>,
    pub name: String,
    pub identity: u64,
    pub transport: Arc<ProcessTransport>,
}
struct ProcessRecord {
    snapshot: ProcessCallbackSnapshot,
    state: ProcessOwnerState,
}
#[derive(Default)]
pub(crate) struct ProcessRegistry {
    records: BTreeMap<u64, ProcessRecord>,
}
impl ProcessRegistry {
    pub(in crate::core) fn shutdown(&mut self) {
        let records = std::mem::take(&mut self.records);
        for record in records.values() {
            record.snapshot.transport.terminate();
        }
        drop(records);
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ProcessRootField {
    Filter,
    Sentinel,
    Buffer,
    Plist,
}
impl Context<'_> {
    /// SAFETY: all reachable local values in callbacks/buffer belong to this
    /// Context, or are audited global values; input values stay live until root registration.
    pub(crate) unsafe fn register_process(
        &self,
        name: String,
        transport: Arc<ProcessTransport>,
        filter: Object,
        sentinel: Object,
        buffer: Object,
    ) -> Result<Object<'_>> {
        ensure!(
            !self
                .process_registry
                .borrow()
                .records
                .values()
                .any(|p| !p.state.sentinel_delivered && p.snapshot.name == name),
            "Process name already exists"
        );
        let process = self.add(LispProcess::new(name.clone(), transport.clone(), self));
        let typed: Gc<&LispProcess> = process.try_into()?;
        // SAFETY: caller establishes callback provenance; process was just allocated here.
        let snapshot = unsafe {
            ProcessCallbackSnapshot {
                _owner_only: std::marker::PhantomData,
                process: Arc::new(HeapDomainValue::new(process, self)),
                filter: Arc::new(HeapDomainValue::new(filter, self)),
                sentinel: Arc::new(HeapDomainValue::new(sentinel, self)),
                buffer: Arc::new(HeapDomainValue::new(buffer, self)),
                plist: Arc::new(HeapDomainValue::new(NIL, self)),
                name,
                identity: typed.identity(),
                transport,
            }
        };
        self.process_registry.borrow_mut().records.insert(
            typed.identity(),
            ProcessRecord { snapshot, state: ProcessOwnerState::default() },
        );
        Ok(process)
    }
    pub(crate) fn process_callback_snapshot(
        &self,
        process: &LispProcess,
    ) -> Result<ProcessCallbackSnapshot> {
        process.check(self)?;
        self.process_registry
            .borrow()
            .records
            .get(&process.identity())
            .map(|p| p.snapshot.clone())
            .ok_or_else(|| anyhow::anyhow!("Process is no longer registered"))
    }
    pub(crate) fn process_snapshots(&self) -> Vec<ProcessCallbackSnapshot> {
        self.process_registry
            .borrow()
            .records
            .values()
            .filter(|p| !p.state.sentinel_delivered)
            .map(|p| p.snapshot.clone())
            .collect()
    }
    pub(crate) fn process_list_roots(&self) -> Vec<Arc<HeapDomainValue>> {
        self.process_registry
            .borrow()
            .records
            .values()
            .filter(|p| !p.state.sentinel_delivered)
            .map(|p| p.snapshot.process.clone())
            .collect()
    }
    pub(crate) fn find_process(&self, name: &str) -> Option<Arc<HeapDomainValue>> {
        self.process_registry
            .borrow()
            .records
            .values()
            .find(|p| !p.state.sentinel_delivered && p.snapshot.name == name)
            .map(|p| p.snapshot.process.clone())
    }
    /// Native-only closure: MUST NOT evaluate Lisp, collect, or retain owner borrows.
    pub(crate) fn update_process_state<R>(
        &self,
        process: &LispProcess,
        update: impl FnOnce(&mut ProcessOwnerState) -> R,
    ) -> Result<R> {
        process.check(self)?;
        let mut registry = self.process_registry.borrow_mut();
        let record = registry
            .records
            .get_mut(&process.identity())
            .ok_or_else(|| anyhow::anyhow!("Process is no longer registered"))?;
        Ok(update(&mut record.state))
    }
    pub(crate) fn process_state(&self, process: &LispProcess) -> Result<ProcessOwnerState> {
        self.update_process_state(process, |state| state.clone())
    }
    /// SAFETY: value's reachable graph has this owner/global provenance and stays live during registration.
    pub(crate) unsafe fn set_process_root(
        &self,
        process: &LispProcess,
        field: ProcessRootField,
        value: Object,
    ) -> Result<()> {
        process.check(self)?;
        ensure!(
            self.process_registry.borrow().records.contains_key(&process.identity()),
            "Process is no longer registered"
        );
        let root = Arc::new(unsafe { HeapDomainValue::new(value, self) });
        let old = {
            let mut registry = self.process_registry.borrow_mut();
            let record = registry.records.get_mut(&process.identity()).unwrap();
            let target = match field {
                ProcessRootField::Filter => &mut record.snapshot.filter,
                ProcessRootField::Sentinel => &mut record.snapshot.sentinel,
                ProcessRootField::Buffer => &mut record.snapshot.buffer,
                ProcessRootField::Plist => &mut record.snapshot.plist,
            };
            std::mem::replace(target, root)
        };
        drop(old);
        Ok(())
    }
    /// SAFETY: same-owner/global value graph, retained until registration.
    pub(crate) unsafe fn set_process_filter(&self, p: &LispProcess, value: Object) -> Result<()> {
        unsafe { self.set_process_root(p, ProcessRootField::Filter, value) }
    }
    /// SAFETY: same-owner/global value graph, retained until registration.
    pub(crate) unsafe fn set_process_sentinel(&self, p: &LispProcess, value: Object) -> Result<()> {
        unsafe { self.set_process_root(p, ProcessRootField::Sentinel, value) }
    }
    /// SAFETY: same-owner/global value graph, retained until registration.
    pub(crate) unsafe fn set_process_buffer(&self, p: &LispProcess, value: Object) -> Result<()> {
        unsafe { self.set_process_root(p, ProcessRootField::Buffer, value) }
    }
    /// SAFETY: same-owner/global value graph, retained until registration.
    pub(crate) unsafe fn set_process_plist(&self, p: &LispProcess, value: Object) -> Result<()> {
        unsafe { self.set_process_root(p, ProcessRootField::Plist, value) }
    }
    pub(crate) fn remove_process(&self, process: &LispProcess) -> Result<()> {
        process.check(self)?;
        let record = self.process_registry.borrow_mut().records.remove(&process.identity());
        if let Some(record) = record {
            record.snapshot.transport.terminate();
            drop(record);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::gc::RootSet,
        process_transport::{ProcessStatus, SpawnSpec},
    };
    use rune_core::macros::root;
    fn cat() -> Arc<ProcessTransport> {
        ProcessTransport::spawn(SpawnSpec {
            program: "/bin/cat".into(),
            args: vec![],
            cwd: None,
            env: vec![],
        })
        .unwrap()
    }
    fn wait_exit(transport: &ProcessTransport) {
        let start = std::time::Instant::now();
        while transport.status() == ProcessStatus::Running {
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn callback_snapshots_remain_rooted_across_gc_replacement_and_removal() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let native = cat();
        let value = unsafe {
            cx.register_process(
                "gc-process".into(),
                native.clone(),
                cx.add("old filter"),
                cx.add("sentinel"),
                NIL,
            )
            .unwrap()
        };
        root!(process, value, cx);
        let typed: Gc<&LispProcess> = process.bind(cx).try_into().unwrap();
        let before = process.bind(cx).into_raw().addr();
        let old = typed.callback_snapshot(cx).unwrap();
        unsafe {
            cx.set_process_filter(&typed, cx.add("new filter")).unwrap();
        }
        for _ in 0..5 {
            cx.garbage_collect(true);
        }
        assert_ne!(before, process.bind(cx).into_raw().addr());
        assert_eq!(old.filter.bind(cx).unwrap().to_string(), "\"old filter\"");
        let typed: Gc<&LispProcess> = process.bind(cx).try_into().unwrap();
        assert_eq!(
            typed.callback_snapshot(cx).unwrap().filter.bind(cx).unwrap().to_string(),
            "\"new filter\""
        );
        cx.remove_process(&typed).unwrap();
        for _ in 0..3 {
            cx.garbage_collect(true);
        }
        assert_eq!(old.sentinel.bind(cx).unwrap().to_string(), "\"sentinel\"");
        assert!(cx.process_list_roots().is_empty());
        wait_exit(&native);
    }
    #[test]
    fn copied_process_is_native_only_and_rejects_foreign_owner_after_teardown() {
        let native = cat();
        let copied = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let value = unsafe {
                cx.register_process(
                    "source".into(),
                    native.clone(),
                    cx.add("private filter"),
                    NIL,
                    NIL,
                )
                .unwrap()
            };
            let process: Gc<&LispProcess> = value.try_into().unwrap();
            LispProcess(GcHeap::new(ProcessInner { data: process.0.data.clone() }, false))
        };
        wait_exit(&native);
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let value = cx.add(copied);
        root!(process, value, cx);
        for _ in 0..3 {
            cx.garbage_collect(true);
        }
        let process: Gc<&LispProcess> = process.bind(cx).try_into().unwrap();
        assert!(process.transport(cx).is_err());
        assert!(process.callback_snapshot(cx).is_err());
        assert_eq!(process.name(), "source");
        assert!(cx.process_snapshots().is_empty());
    }
    #[test]
    fn unreachable_process_wrapper_releases_its_native_arc() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let native = cat();
        let weak = Arc::downgrade(&native);
        let _unrooted = cx.add(LispProcess::new("unrooted".into(), native, cx));
        cx.garbage_collect(true);
        assert!(weak.upgrade().is_none(), "collector must finalize native Arc exactly once");
        cx.garbage_collect(true);
    }
}

#[cfg(test)]
mod retired_snapshot_tests {
    use super::*;
    use crate::{
        core::gc::RootSet,
        process_transport::{ProcessStatus, SpawnSpec},
    };
    #[test]
    fn retained_snapshot_after_owner_teardown_cannot_bind_new_context() {
        let native = ProcessTransport::spawn(SpawnSpec {
            program: "/bin/cat".into(),
            args: vec![],
            cwd: None,
            env: vec![],
        })
        .unwrap();
        let snapshot = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let value = unsafe {
                cx.register_process(
                    "retired".into(),
                    native.clone(),
                    cx.add("private callback"),
                    NIL,
                    NIL,
                )
                .unwrap()
            };
            let process: Gc<&LispProcess> = value.try_into().unwrap();
            process.callback_snapshot(&cx).unwrap()
        };
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        assert!(snapshot.filter.bind(&cx).is_err());
        assert!(snapshot.process.bind(&cx).is_err());
        let start = std::time::Instant::now();
        while native.status() == ProcessStatus::Running {
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod completed_metadata_tests {
    use super::*;
    use crate::{
        core::gc::RootSet,
        process_transport::{ProcessStatus, SpawnSpec},
    };
    use rune_core::macros::root;
    #[test]
    fn completed_process_keeps_metadata_but_leaves_active_lookup_and_allows_name_reuse() {
        let spawn = || {
            ProcessTransport::spawn(SpawnSpec {
                program: "/bin/cat".into(),
                args: vec![],
                cwd: None,
                env: vec![],
            })
            .unwrap()
        };
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let native = spawn();
        let value = unsafe {
            cx.register_process(
                "reusable".into(),
                native.clone(),
                cx.add("retained metadata"),
                NIL,
                NIL,
            )
            .unwrap()
        };
        root!(old, value, cx);
        native.terminate();
        let start = std::time::Instant::now();
        while native.status() == ProcessStatus::Running {
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let typed: Gc<&LispProcess> = old.bind(cx).try_into().unwrap();
        cx.update_process_state(&typed, |state| {
            state.stdout_eof = true;
            state.stderr_eof = true;
            state.sentinel_delivered = true;
        })
        .unwrap();
        assert!(cx.find_process("reusable").is_none());
        assert!(cx.process_list_roots().is_empty());
        assert!(cx.process_snapshots().is_empty());
        let replacement = spawn();
        let value = unsafe {
            cx.register_process("reusable".into(), replacement.clone(), NIL, NIL, NIL)
                .unwrap()
        };
        root!(new, value, cx);
        for _ in 0..3 {
            cx.garbage_collect(true);
        }
        let typed: Gc<&LispProcess> = old.bind(cx).try_into().unwrap();
        assert_eq!(
            typed.callback_snapshot(cx).unwrap().filter.bind(cx).unwrap().to_string(),
            "\"retained metadata\""
        );
        let fresh: Gc<&LispProcess> = new.bind(cx).try_into().unwrap();
        assert_ne!(typed.identity(), fresh.identity());
        assert_eq!(cx.process_list_roots().len(), 1);
        cx.remove_process(&typed).unwrap();
        cx.remove_process(&fresh).unwrap();
    }
}
