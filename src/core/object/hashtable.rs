//! The design of the hashtable is based on the requirments we have. First is we
//! need it to support being both thread local and global. Second we need
//! iterate and mutate at the same time. Third we need to be able to clean up
//! the heap allocation when it is garbage collected.
use super::hash_key::{SemanticKey,validate_keys,ensure_portable_key};
use super::{CloneIn, Gc, HashTableTest, IntoObject, ObjCell, Object, WithLifetime};
use anyhow::{Result,ensure};
use std::marker::PhantomData;
use crate::core::env::INTERNED_SYMBOLS;
use crate::core::gc::{Block, GcHeap, GcState, Trace};
use crate::derive_GcMoveable;
use rune_core::hashmap::{HashSet, IndexMap};
use rune_macros::Trace;
use std::cell::RefCell;
use std::fmt::{self, Debug, Display, Write};
use std::ptr::NonNull;
use std::sync::Mutex;

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub(crate) enum HashTableWeakness {Strong,Key,Value}
/// Actual stored cells, not a layout cast between generic IndexMap types.
pub(crate) struct HashTable<'a>{test:HashTableTest,weakness:HashTableWeakness,inner:IndexMap<SemanticKey,ObjCell>,_arena:PhantomData<Object<'a>>}
impl Default for HashTable<'_>{fn default()->Self{Self::with_test(HashTableTest::Equal)}}
impl<'a> HashTable<'a>{
 pub(crate) fn with_test(test:HashTableTest)->Self{Self::with_test_and_weakness(test,HashTableWeakness::Strong)}
 pub(crate) fn with_test_and_weakness(test:HashTableTest,weakness:HashTableWeakness)->Self{Self{test,weakness,inner:IndexMap::default(),_arena:PhantomData}}
 pub(crate) fn test(&self)->HashTableTest{self.test}
 pub(crate) fn weakness(&self)->HashTableWeakness{self.weakness}
 pub(crate) fn len(&self)->usize{self.inner.len()}
 fn validate(&self,key:Object)->Result<()>{validate_keys(self.test,self.iter().map(|(k,_)|k).chain(std::iter::once(key)))}
 pub(crate) fn insert(&mut self,key:Object<'a>,value:Object<'a>)->Result<Option<Object<'a>>>{
  self.validate(key)?;ensure!(self.len()<65536||self.get(&key)?.is_some(),"Hash table entry limit exceeded");self.inner.try_reserve(1)?;
  let old=self.inner.insert(unsafe{SemanticKey::new(key,self.test)},unsafe{ObjCell::new(value)});
  Ok(old.map(|value|unsafe{value.get().with_lifetime()}))
 }
 pub(crate) fn get(&self,key:&Object)->Result<Option<Object<'a>>>{self.validate(*key)?;let key=unsafe{SemanticKey::new(*key,self.test)};Ok(self.inner.get(&key).map(|v|unsafe{v.get().with_lifetime()}))}
 pub(crate) fn get_index_of(&self,key:&Object)->Result<Option<usize>>{self.validate(*key)?;let key=unsafe{SemanticKey::new(*key,self.test)};Ok(self.inner.get_index_of(&key))}
 pub(crate) fn shift_remove(&mut self,key:&Object)->Result<Option<Object<'a>>>{self.validate(*key)?;let key=unsafe{SemanticKey::new(*key,self.test)};Ok(self.inner.shift_remove(&key).map(|v|unsafe{v.get().with_lifetime()}))}
 pub(crate) fn get_index(&self,index:usize)->Option<(Object<'a>,Object<'a>)>{self.inner.get_index(index).map(|(k,v)|unsafe{(k.value.get().with_lifetime(),v.get().with_lifetime())})}
 pub(crate) fn iter(&self)->impl Iterator<Item=(Object<'a>,Object<'a>)>+'_ {self.inner.iter().map(|(k,v)|unsafe{(k.value.get().with_lifetime(),v.get().with_lifetime())})}
}

#[derive(PartialEq, Trace)]
pub(crate) struct LispHashTable(GcHeap<HashTableCore<'static>>);

impl Eq for LispHashTable {}

derive_GcMoveable!(LispHashTable);

impl Debug for LispHashTable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.display_walk(f, &mut HashSet::default())
    }
}

impl Display for LispHashTable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.display_walk(f, &mut HashSet::default())
    }
}

impl LispHashTable {
    /// Snapshot exact recursive clone edges under one table guard. The caller
    /// must exclude collection/callbacks while retaining the borrowed objects.
    pub(crate) fn clone_children_snapshot(&self, limit: usize) -> anyhow::Result<Vec<Object<'_>>> {
        self.0.with(|table| {
            let count = table
                .len()
                .checked_mul(2)
                .ok_or_else(|| anyhow::anyhow!("hash-table clone edge count overflow"))?;
            anyhow::ensure!(count <= limit, "hash-table clone edge budget exceeded");
            let mut children = Vec::new();
            children.try_reserve_exact(count)?;
            for (key, value) in table.iter() {
                children.extend([key,value]);
            }
            Ok(children)
        })
    }
    pub(in crate::core) unsafe fn new(table: HashTable, constant: bool) -> Self {
        Self(GcHeap::new(HashTableCore::new(table, constant), constant))
    }

    pub(in crate::core) fn forwarding_ptr(&self) -> Option<NonNull<u8>> {
        use crate::core::gc::AllocState as A;
        match self.0.allocation_state() {
            A::Forwarded(f) => Some(f),
            A::Global => panic!("global hashtable allocation found in local heap"),
            A::Unmoved => None,
        }
    }
}

pub(in crate::core) struct HashTableCore<'ob>(HashTableType<'ob>);

// Hashtables are currently the only data structure that can be shared between
// threads. This is because it is used for caching in some functions in
// cl-generic.
enum HashTableType<'a> {
    Local(RefCell<HashTableInner<'a>>),
    Global(Mutex<HashTableInner<'a>>),
}

struct HashTableInner<'ob> {
    // The current index of a [`maphash`] iterator. This is needed because we
    // can't hold the hashtable across calls to elisp (it might mutate it).
    iter_idx: usize,
    inner: HashTable<'ob>,
}

impl LispHashTable {
    pub(crate) fn test(&self)->HashTableTest{self.0.with(|x|x.test())}
    pub(crate) fn weakness(&self)->HashTableWeakness{self.0.with(|x|x.weakness())}
    pub(crate) fn len(&self) -> usize {
        self.0.with(|x| x.len())
    }

    pub(crate) fn get(&self, key: Object) -> Result<Option<Object<'_>>> {
        self.0.with(|x| x.get(&key))
    }

    pub(crate) fn get_index(&self, index: usize) -> Option<(Object<'_>, Object<'_>)> {
        self.0.with(|x| x.get_index(index))
    }

    pub(crate) fn get_index_of(&self, key: Object) -> Result<Option<usize>> {
        self.0.with(|x| x.get_index_of(&key))
    }

    pub(crate) fn insert(&self, key: Object, value: Object) -> anyhow::Result<()> {
        self.insert_inner(key, value, None)
    }
    #[cfg(test)]
    pub(crate) fn insert_with_copy_failure(
        &self,
        key: Object,
        value: Object,
        after: usize,
    ) -> anyhow::Result<()> {
        self.insert_inner(key, value, Some(after))
    }
    fn insert_inner(
        &self,
        key: Object,
        value: Object,
        _copy_failure_after: Option<usize>,
    ) -> anyhow::Result<()> {
        let mut deferred = crate::core::gc::DeferredEndpointFinalizers::default();
        match &self.0.0 {
            HashTableType::Local(table) => {
                let key = unsafe { key.with_lifetime() };
                let value = unsafe { value.with_lifetime() };
                table.borrow_mut().inner.insert(key, value)?
            }
            HashTableType::Global(table) => {
                let map = INTERNED_SYMBOLS.lock().unwrap();
                let block = map.global_block();
                #[cfg(test)]
                let _failure_scope =
                    _copy_failure_after.map(|after| block.scoped_copy_failure(after));
                // Keys and values share one copy session, preserving aliases
                // inside this insertion without retaining stale external IDs.
                // SAFETY: target copy is exclusive and unpublished until commit; deferred outlives locks.
                let transaction = unsafe { block.copy_transaction(&mut deferred) };
                let _clone_session = block.clone_session();
                ensure_portable_key(self.test(),key)?;
                // Need to clone these objects in the global block since this
                // hashtable is globally shared
                let key = unsafe { key.clone_in(block)?.with_lifetime() };
                let value = unsafe { value.clone_in(block)?.with_lifetime() };
                let previous = table.lock().unwrap().inner.insert(key, value)?;
                transaction.commit();
                previous
            }
        };
        Ok(())
    }

    pub(crate) fn shift_remove(&self, key: Object)->Result<Option<Object<'_>>> {
        let key = unsafe { key.with_lifetime() };
        self.0.with(|x| x.shift_remove(&key))
    }

    pub(crate) fn get_iter_index(&self) -> usize {
        match &self.0.0 {
            HashTableType::Local(table) => table.borrow().iter_idx,
            HashTableType::Global(table) => table.lock().unwrap().iter_idx,
        }
    }

    pub(crate) fn set_iter_index(&self, index: usize) {
        match &self.0.0 {
            HashTableType::Local(table) => table.borrow_mut().iter_idx = index,
            HashTableType::Global(table) => table.lock().unwrap().iter_idx = index,
        }
    }
}

impl<'a> HashTableCore<'a> {
    unsafe fn new(table: HashTable, constant: bool) -> Self {
        // SAFETY: the constructor caller owns the complete source graph for this arena.
        let HashTable{test,weakness,inner,..}=table;
        assert!(!constant||weakness==HashTableWeakness::Strong,"weak tables require owner-local allocation");
        let table=HashTable{test,weakness,inner,_arena:PhantomData};
        let inner = HashTableInner { iter_idx: 0, inner: table };
        if constant {
            HashTableCore(HashTableType::Global(Mutex::new(inner)))
        } else {
            HashTableCore(HashTableType::Local(RefCell::new(inner)))
        }
    }

    fn with<F, T>(&self, mut f: F) -> T
    where
        F: FnMut(&mut HashTable<'a>) -> T,
    {
        match &self.0 {
            HashTableType::Local(table) => f(&mut table.borrow_mut().inner),
            HashTableType::Global(table) => f(&mut table.lock().unwrap().inner),
        }
    }
}

impl Trace for HashTableCore<'_> {
    fn trace(&self, state: &mut GcState) {
        let HashTableType::Local(table) = &self.0 else {
            panic!("Global hash table should not be traced")
        };
        if table.borrow().inner.weakness!=HashTableWeakness::Strong {
            // self is the live moved payload; no table borrow escapes enrollment.
            state.defer_weak_table(NonNull::from(self).cast());return;
        }
        table.borrow_mut().inner.inner.rehash_keys(|key,value|{key.trace(state);value.trace(state);});
    }
}

impl PartialEq for HashTableCore<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl<'new> CloneIn<'new, &'new Self> for LispHashTable {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> anyhow::Result<Gc<&'new Self>> {
        ensure!(self.weakness()==HashTableWeakness::Strong,"Weak table cross-arena copies are unsupported");
        let mut table=HashTable::with_test(self.test());
        self.0.with(|x| -> anyhow::Result<()> {
            for (key, value) in x.iter() {
                ensure_portable_key(x.test(),key)?;
                let new_key = key.clone_in(bk)?;
                let new_value = value.clone_in(bk)?;
                table.insert(new_key, new_value)?;
            }
            Ok(())
        })?;
        Ok(table.into_obj(bk))
    }
}

impl LispHashTable {
    pub(super) fn display_walk(
        &self,
        f: &mut fmt::Formatter,
        seen: &mut HashSet<*const u8>,
    ) -> fmt::Result {
        let ptr = (&*self.0 as *const HashTableCore).cast();
        if seen.contains(&ptr) {
            return write!(f, "#0");
        }
        seen.insert(ptr);

        write!(f,"#s(hash-table test {}",self.test().name())?;
        match self.weakness(){HashTableWeakness::Strong=>{},HashTableWeakness::Key=>write!(f," weakness key")?,HashTableWeakness::Value=>write!(f," weakness value")?}
        write!(f," data (")?;
        self.0.with(|x| -> fmt::Result {
            for (i, (k, v)) in x.iter().enumerate() {
                if i != 0 {
                    f.write_char(' ')?;
                }
                k.untag().display_walk(f, seen)?;
                f.write_char(' ')?;
                v.untag().display_walk(f, seen)?;
            }
            Ok(())
        })?;
        write!(f, "))")
    }
}

impl HashTableCore<'_>{
 /// Do not rewrite stored weak cells during the fixed point: all still refer to
 /// from-space/global headers. Temporary cells mark the conditional opposite edge.
 pub(in crate::core) fn mark_weak_edges(&self,state:&mut GcState){
  let entries=self.with(|t|t.iter().collect::<Vec<_>>());let weakness=self.with(|t|t.weakness);
  for(key,value)in entries{
   let activate=match weakness{HashTableWeakness::Key=>key.weak_forwarded().is_some(),HashTableWeakness::Value=>value.weak_forwarded().is_some(),HashTableWeakness::Strong=>unreachable!()};
   if activate{let edge=match weakness{HashTableWeakness::Key=>value,_=>key};let cell=unsafe{ObjCell::new(edge)};cell.trace(state);}
  }
 }
 pub(in crate::core) fn finish_weak_edges(&self){
  let HashTableType::Local(cell)=&self.0 else{unreachable!("weak global table")};
  let mut borrowed=cell.borrow_mut();let HashTableInner{iter_idx,inner:t}=&mut *borrowed;
  // Fork shift_remove_index uses stored hash/index only, not key Eq/Hash.
  for i in (0..t.inner.len()).rev(){let(key,value)=t.inner.get_index(i).unwrap();let live=match t.weakness{HashTableWeakness::Key=>key.value.get().weak_forwarded().is_some(),HashTableWeakness::Value=>value.get().weak_forwarded().is_some(),HashTableWeakness::Strong=>unreachable!()};if !live{t.inner.shift_remove_index(i);if i<*iter_idx{*iter_idx-=1;}}}
  t.inner.rehash_keys(|key,value|unsafe{key.value.as_mut().set(key.value.get().weak_forwarded().expect("activated weak key"));value.as_mut().set(value.get().weak_forwarded().expect("activated weak value"));});
 }
}
#[cfg(test)]
#[path="hash_strategy_recovery_tests.rs"]
mod strategy_recovery_tests;
#[cfg(test)]
#[path="weak_hash_recovery_tests.rs"]
mod weak_recovery_tests;
