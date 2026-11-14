use super::{CloneIn, Gc, IntoObject, NIL, Object, WithLifetime};
use crate::{
    core::gc::{AllocState, Block, GcHeap, GcState, Slot, Trace as TraceTrait},
    derive_GcMoveable,
};
use anyhow::{Result, ensure};
use rune_core::hashmap::HashMap;
use rune_macros::Trace;
use std::{
    fmt,
    sync::{Mutex, MutexGuard},
};

pub struct CharTableInner<'ob> {
    parent: Mutex<Option<Slot<&'ob CharTable>>>,
    data: Mutex<HashMap<usize, Slot<Object<'ob>>>>,
    init: Slot<Object<'ob>>,
}

#[cfg(test)]
thread_local! { static FINALIZED_CHAR_TABLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
impl Drop for CharTableInner<'_> {
    fn drop(&mut self) {
        FINALIZED_CHAR_TABLES.with(|count| count.set(count.get() + 1));
    }
}
#[cfg(test)]
pub(crate) fn finalized_char_table_count() -> usize {
    FINALIZED_CHAR_TABLES.with(std::cell::Cell::get)
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl fmt::Debug for CharTableInner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Diagnostics must not recursively format values under these locks.
        f.debug_struct("CharTableInner").finish_non_exhaustive()
    }
}

impl TraceTrait for CharTableInner<'_> {
    fn trace(&self, state: &mut GcState) {
        lock(&self.parent).trace(state);
        lock(&self.data).trace(state);
        self.init.trace(state);
    }
}

impl<'ob> CharTableInner<'ob> {
    pub fn new(init: Option<Object<'ob>>) -> Self {
        CharTableInner {
            parent: Mutex::new(None),
            data: Mutex::new(HashMap::default()),
            init: Slot::new(init.unwrap_or(NIL)),
        }
    }
}

#[derive(PartialEq, Eq, Trace, Debug)]
pub(crate) struct CharTable(GcHeap<CharTableInner<'static>>);

derive_GcMoveable!(CharTable);

impl Eq for CharTableInner<'_> {}

impl PartialEq for CharTableInner<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl<'new> CloneIn<'new, &'new Self> for CharTable {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> anyhow::Result<Gc<&'new Self>> {
        // Release source locks before recursively cloning. The source borrow
        // excludes local collection; globally published tables are immutable.
        let source_parent = lock(&self.0.parent).as_ref().map(|parent| **parent);
        let source_data: Vec<_> =
            lock(&self.0.data).iter().map(|(&key, value)| (key, **value)).collect();
        let parent_clone = source_parent
            .map(|parent| Ok::<_, anyhow::Error>(Slot::new(parent.clone_in(bk)?.untag())))
            .transpose()?;
        let parent = Mutex::new(parent_clone);
        let mut data = HashMap::default();
        for (key, value) in source_data {
            data.insert(key, Slot::new(value.clone_in(bk)?));
        }
        let data = Mutex::new(data);
        let init = Slot::new(self.0.init.clone_in(bk)?);
        Ok(CharTableInner { parent, data, init }.into_obj(bk))
    }
}

impl CharTable {
    /// Borrowed clone edges only. Caller must not collect/evaluate while these
    /// objects are retained; no lock escapes into recursive graph traversal.
    pub(crate) fn clone_children_snapshot(&self, limit: usize) -> Result<Vec<Object<'_>>> {
        let parent = lock(&self.0.parent);
        let data = lock(&self.0.data);
        let count = data
            .len()
            .checked_add(1 + usize::from(parent.is_some()))
            .ok_or_else(|| anyhow::anyhow!("char-table clone edge count overflow"))?;
        ensure!(count <= limit, "char-table clone edge budget exceeded");
        let mut children = Vec::new();
        children.try_reserve_exact(count)?;
        if let Some(parent) = parent.as_ref() {
            children.push(Object::from(**parent));
        }
        children.push(*self.0.init);
        children.extend(data.values().map(|value| **value));
        Ok(children)
    }
    pub(in crate::core) unsafe fn new(table: CharTableInner<'_>, constant: bool) -> Self {
        // transmute lifetime to static
        let table =
            unsafe { std::mem::transmute::<CharTableInner<'_>, CharTableInner<'static>>(table) };
        Self(GcHeap::new(table, constant))
    }

    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            AllocState::Forwarded(pointer) => Some(pointer),
            AllocState::Unmoved => None,
            AllocState::Global => panic!("global char-table found in local finalization list"),
        }
    }

    pub fn get(&self, idx: usize) -> Object<'_> {
        match lock(&self.0.data).get(&idx) {
            Some(x) => **x,
            None => *self.0.init,
        }
    }

    fn check_writable(&self) -> Result<()> {
        ensure!(
            matches!(self.0.allocation_state(), AllocState::Unmoved),
            "Attempt to modify an immutable or forwarded char-table"
        );
        Ok(())
    }

    pub fn set(&self, idx: usize, item: Object) -> Result<()> {
        self.check_writable()?;
        unsafe { lock(&self.0.data).insert(idx, Slot::new(item.with_lifetime())) };
        Ok(())
    }

    pub fn set_parent(&self, new: Option<&Self>) -> Result<()> {
        self.check_writable()?;
        let new_ptr = new.map(|n| unsafe { Slot::new(n.with_lifetime()) });
        *lock(&self.0.parent) = new_ptr;
        Ok(())
    }
}

impl fmt::Display for CharTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        // Copy stable borrowed values, then release the mutex before formatting
        // them; a value can refer back to this table. General cyclic formatting
        // remains unsupported, but must not become a mutex reentry deadlock.
        let mut entries: Vec<_> =
            lock(&self.0.data).iter().map(|(&key, value)| (key, **value)).collect();
        entries.sort_by_key(|&(key, _)| key);

        let mut iter = entries.into_iter();
        if let Some((_, first)) = iter.next() {
            write!(f, "{first}")?;
            for (_, value) in iter {
                write!(f, " {value}")?;
            }
        }
        write!(f, "]")
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::core::{
        cons::Cons,
        env::{INTERNED_SYMBOLS, intern},
        gc::{Context, RootSet},
        object::{Function, FunctionType, ObjectType},
    };
    use rune_core::macros::root;

    #[test]
    fn local_table_parent_and_values_survive_repeated_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let table = CharTableInner::new(Some(cx.add("default"))).into_obj(&cx.block);
        root!(table, cx);
        {
            let parent = CharTableInner::new(None).into_obj(&cx.block);
            parent.untag().set(66, cx.add("parent-only")).unwrap();
            table.bind(cx).untag().set_parent(Some(parent.untag())).unwrap();
            table.bind(cx).untag().set(65, cx.add("entry-only")).unwrap();
        }
        for _ in 0..10 {
            cx.garbage_collect(true);
            let table = table.bind(cx).untag();
            assert_eq!(table.get(65), "entry-only");
            assert_eq!(table.get(0), "default");
            let parent = lock(&table.0.parent).as_ref().map(|parent| **parent).unwrap();
            assert_eq!(parent.get(66), "parent-only");
        }
    }

    #[test]
    fn collector_forwards_and_finalizes_native_char_table_resources() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        {
            let table = CharTableInner::new(None).into_obj(&cx.block);
            root!(table, cx);
            table.bind(cx).untag().set(1, cx.add("native map entry")).unwrap();
            assert_eq!(cx.block.lisp_chartables.borrow().len(), 1);
            for _ in 0..5 {
                cx.garbage_collect(true);
                assert_eq!(cx.block.lisp_chartables.borrow().len(), 1);
                assert_eq!(table.bind(cx).untag().get(1), "native map entry");
            }
        }
        cx.garbage_collect(true);
        assert!(cx.block.lisp_chartables.borrow().is_empty());
    }

    #[test]
    fn cyclic_local_table_finalizes_exactly_once_after_last_root_drops() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let before = finalized_char_table_count();
        {
            let table = CharTableInner::new(None).into_obj(&cx.block);
            root!(table, cx);
            let current = table.bind(cx);
            current.untag().set_parent(Some(current.untag())).unwrap();
            current.untag().set(1, cx.add(current)).unwrap();
            for _ in 0..5 {
                cx.garbage_collect(true);
                assert_eq!(finalized_char_table_count(), before);
                assert_eq!(cx.block.tracked_char_table_allocations(), 1);
            }
        }
        cx.garbage_collect(true);
        assert_eq!(finalized_char_table_count(), before + 1);
        assert_eq!(cx.block.tracked_char_table_allocations(), 0);
        cx.garbage_collect(true);
        assert_eq!(finalized_char_table_count(), before + 1);
    }

    #[test]
    fn globally_published_table_is_readonly_and_concurrently_readable() {
        let name = "gc-ownership-published-char-table";
        {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let table = CharTableInner::new(Some(cx.add("global-default"))).into_obj(&cx.block);
            table.untag().set(42, cx.add("global-entry")).unwrap();
            let definition = Cons::new(table, NIL, &cx);
            let function: Function = cx.add(definition).try_into().unwrap();
            let symbol = intern(name, &cx);
            crate::data::fset(symbol, function.into()).unwrap();
            // Global function constants must not retain the source local table.
            table.untag().set(42, cx.add("changed-local")).unwrap();
        }
        let readers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    let roots = RootSet::default();
                    let cx = Context::new(&roots);
                    for _ in 0..200 {
                        let function = intern(name, &cx).func(&cx).unwrap();
                        let FunctionType::Cons(definition) = function.untag() else {
                            panic!("cons expected")
                        };
                        let ObjectType::CharTable(table) = definition.car().untag() else {
                            panic!("table expected")
                        };
                        assert_eq!(table.get(42), "global-entry");
                        assert_eq!(table.get(0), "global-default");
                        assert!(table.set(42, cx.add("foreign-local")).is_err());
                        assert!(table.set_parent(None).is_err());
                        assert!(format!("{table}").contains("global-entry"));
                        assert_eq!(table.get(42), "global-entry");
                    }
                })
            })
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
    }

    #[test]
    fn diagnostics_do_not_reenter_data_or_parent_locks() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let table = CharTableInner::new(None).into_obj(&cx.block);
        let table = table.untag();
        let _data = lock(&table.0.data);
        let _parent = lock(&table.0.parent);
        assert!(format!("{table:?}").contains("CharTableInner"));
    }
}
