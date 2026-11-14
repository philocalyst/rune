use super::GcState;
use super::Trace;
use crate::core::object::GcString;
use crate::core::object::WeakBufferPropertyRoot;
use crate::core::object::{CharTable, LispHashTable};
use crate::core::object::{Gc, IntoObject, Object, UninternedSymbolMap, WithLifetime};
use bumpalo::collections::Vec as GcVec;
use std::cell::{Cell, RefCell};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A global store of all gc roots. This struct should be passed to the [Context]
/// when it is created.
#[derive(Default, Debug)]
pub(crate) struct RootSet {
    pub(super) roots: RefCell<Vec<*const dyn Trace>>,
}

/// Thread-safe version of RootSet that uses Mutex instead of RefCell.
/// This is specifically designed for multi-threaded scenarios like ChannelManager
/// where roots need to be managed across thread boundaries.
///
/// Note: Send/Sync safety is guaranteed by HeapRoot, which ensures the raw pointers
/// only point to data it owns in HeapRoot::data.
#[derive(Debug)]
pub(crate) struct ThreadSafeRootSet {
    pub(super) roots: std::sync::Mutex<Vec<*const dyn Trace>>,
}

impl Default for ThreadSafeRootSet {
    fn default() -> Self {
        Self { roots: std::sync::Mutex::new(Vec::new()) }
    }
}

#[expect(dead_code)]
// These types are only stored here so they can be dropped
pub(in crate::core) enum DropStackElem {
    String(String),
    ByteString(Vec<u8>),
    Vec(Vec<Object<'static>>),
}

/// A block of allocations. This type should be owned by [Context] and not used
/// directly.
pub(crate) struct Block<const CONST: bool> {
    pub(in crate::core) objects: bumpalo::Bump,
    // Allocations that will be dropped when the objects are moved. At that time
    // the allocation will get copied into the GC heap. This let's us avoid an
    // extra copy of memory when a vector is first made an object. The
    // allocation can continue to live outside of the GC heap until we copy the
    // object.
    pub(in crate::core) drop_stack: RefCell<Vec<DropStackElem>>,
    // We don't yet have a hashmap that supports allocators, so we need to keep
    // track of the memory and free it only after the table is garbage
    // collected. Kind of a hack.
    pub(in crate::core) lisp_hashtables: RefCell<Vec<*const LispHashTable>>,
    // Char-tables also own native map/mutex resources requiring finalization.
    pub(in crate::core) lisp_chartables: RefCell<Vec<*const CharTable>>,
    pub(in crate::core) uninterned_symbol_map: UninternedSymbolMap,
}

unsafe impl<const C: bool> Send for Block<C> {}

/// Owns all allocations and creates objects. All objects have
/// a lifetime tied to the borrow of their `Context`. When the
/// `Context` goes out of scope, no objects should be accessible.
pub(crate) struct Context<'rt> {
    pub(crate) block: Block<false>,
    root_set: &'rt RootSet,
    next_limit: usize,
    domain_id: u64,
    buffer_properties: RefCell<Vec<WeakBufferPropertyRoot>>,
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        // Global buffer handles outlive the local heap: retire their Lisp
        // metadata before releasing any allocation in this domain.
        for root in self.buffer_properties.get_mut().drain(..).filter_map(|root| root.upgrade()) {
            root.close_owned(self.domain_id);
        }
        self.garbage_collect(true);
        if self.block.objects.allocated_bytes() == 0 {
            return;
        }
        if std::thread::panicking() {
            eprintln!("Error: Context was dropped while still holding data");
        } else {
            panic!("Error: Context was dropped while still holding data");
        }
    }
}

thread_local! {
    /// Ensure there is only one context per thread.
    static SINGLETON_CHECK: Cell<bool> = const { Cell::new(false) };
}

/// Ensure there is only one global context.
static GLOBAL_CHECK: AtomicBool = AtomicBool::new(false);

impl Block<true> {
    pub(crate) fn new_global() -> Self {
        use std::sync::atomic::Ordering::SeqCst as Ord;
        assert!(GLOBAL_CHECK.compare_exchange(false, true, Ord, Ord).is_ok());
        Self::empty()
    }
}

impl Block<false> {
    pub(crate) fn new_local() -> Self {
        Self::assert_unique();
        Self::empty()
    }

    /// Construct an arena outside the thread's sole ordinary Context.
    ///
    /// SAFETY: The caller must supply an independent owner which prevents raw
    /// borrowed objects escaping into any other heap domain. Transfers must
    /// deep-copy, and collection must synchronize all retained root slots.
    pub(crate) unsafe fn new_local_unchecked() -> Self {
        Self::empty()
    }

    pub(crate) fn assert_unique() {
        SINGLETON_CHECK.with(|x| {
            assert!(!x.get(), "There was already and active context when this context was created");
            x.set(true);
        });
    }
}

impl<const CONST: bool> Block<CONST> {
    // No Default impl: a safe second local arena would bypass Context's domain
    // singleton and let borrowed objects be rooted in the wrong heap.
    fn empty() -> Self {
        Self {
            objects: bumpalo::Bump::new(),
            drop_stack: RefCell::default(),
            lisp_hashtables: RefCell::default(),
            lisp_chartables: RefCell::default(),
            uninterned_symbol_map: UninternedSymbolMap::default(),
        }
    }

    /// Scope the uninterned-symbol alias cache to one complete graph copy.
    pub(crate) fn clone_session(&self) -> impl Drop + '_ {
        self.uninterned_symbol_map.clone_session()
    }

    pub(crate) fn add<'ob, T, Tx>(&'ob self, obj: T) -> Object<'ob>
    where
        T: IntoObject<Out<'ob> = Tx>,
        Gc<Tx>: Into<Object<'ob>>,
    {
        obj.into_obj(self).into()
    }

    pub(crate) fn add_as<'ob, T, Tx, V>(&'ob self, obj: T) -> Gc<V>
    where
        T: IntoObject<Out<'ob> = Tx>,
        Gc<Tx>: Into<Gc<V>>,
    {
        obj.into_obj(self).into()
    }

    /// Create a new String whose backing storage is already part of the GC
    /// heap. Does not require dropping when moved during garbage collection
    /// (unlike std::string).
    pub(crate) fn string_with_capacity(&self, cap: usize) -> GcString<'_> {
        GcString::with_capacity_in(cap, &self.objects)
    }

    /// Create a new Vec whose backing storage is already part of the GC
    /// heap. Does not require dropping when moved during garbage collection
    /// (unlike std::vec).
    pub(crate) fn vec_new(&self) -> GcVec<'_, Object<'_>> {
        GcVec::new_in(&self.objects)
    }

    pub(crate) fn vec_with_capacity(&self, cap: usize) -> GcVec<'_, Object<'_>> {
        GcVec::with_capacity_in(cap, &self.objects)
    }
}

static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);

impl<'ob, 'rt> Context<'rt> {
    pub(crate) const MIN_GC_BYTES: usize = 2000;
    pub(crate) const GC_GROWTH_FACTOR: usize = 12; // divide by 10
    pub(crate) fn new(roots: &'rt RootSet) -> Self {
        let block = Block::new_local();
        Self::with_domain(block, roots)
    }

    fn with_domain(block: Block<false>, roots: &'rt RootSet) -> Self {
        let domain_id = NEXT_DOMAIN
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("Heap domain identifiers exhausted");
        Self {
            block,
            root_set: roots,
            next_limit: Self::MIN_GC_BYTES,
            domain_id,
            buffer_properties: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn domain_id(&self) -> u64 {
        self.domain_id
    }
    pub(crate) fn register_buffer_properties(&self, root: WeakBufferPropertyRoot) {
        self.buffer_properties.borrow_mut().push(root);
    }

    pub(crate) fn bind<T>(&'ob self, obj: T) -> <T as WithLifetime<'ob>>::Out
    where
        T: WithLifetime<'ob>,
    {
        unsafe { obj.with_lifetime() }
    }

    pub(crate) fn get_root_set(&'ob self) -> &'rt RootSet {
        self.root_set
    }

    pub(crate) fn garbage_collect(&mut self, force: bool) {
        let bytes = self.block.objects.allocated_bytes();
        if cfg!(not(test)) && !force && bytes < self.next_limit {
            return;
        }

        self.block.uninterned_symbol_map.clear();
        let mut state = GcState::new();
        for x in self.root_set.roots.borrow().iter() {
            // SAFETY: The contract of root structs will ensure that it removes
            // itself from this list before it drops.
            unsafe {
                (**x).trace(&mut state);
            }
        }

        self.buffer_properties.borrow_mut().retain(|root| {
            if let Some(root) = root.upgrade() {
                root.trace_owned(self.domain_id, &mut state);
                true
            } else {
                false
            }
        });
        state.trace_stack();

        self.next_limit = (state.to_space.allocated_bytes() * Self::GC_GROWTH_FACTOR) / 10;
        self.block.drop_stack.borrow_mut().clear();
        // Find all hashtables that have not been moved (i.e. They are no longer
        // accessible) and drop them. Otherwise, update the object pointer.
        self.block.lisp_hashtables.borrow_mut().retain_mut(|ptr| {
            let table = unsafe { &**ptr };
            if let Some(fwd) = table.forwarding_ptr() {
                *ptr = fwd.as_ptr().cast::<LispHashTable>();
                true
            } else {
                unsafe { std::ptr::drop_in_place(*ptr as *mut LispHashTable) };
                false
            }
        });

        self.block.lisp_chartables.borrow_mut().retain_mut(|ptr| {
            let table = unsafe { &**ptr };
            if let Some(fwd) = table.forwarding_ptr() {
                *ptr = fwd.as_ptr().cast::<CharTable>();
                true
            } else {
                unsafe { std::ptr::drop_in_place(*ptr as *mut CharTable) };
                false
            }
        });

        self.block.objects = state.to_space;
    }
}

/// Perform garbage collection on a block with a root set without requiring a Context.
/// This is useful for scenarios like ChannelManager where we need to GC a shared block
/// from any thread without conflicting with thread-local Context singleton checks.
///
/// # Safety
/// Caller must ensure:
/// 1. Exclusive access to the block (no concurrent reads/writes)
/// 2. The root_set accurately represents all live references to objects in the block
/// 3. The block's drop_stack and native-resource allocation lists are properly maintained
/// 4. No active Context exists that references this block or root_set
pub(crate) unsafe fn collect_garbage_raw(
    block: &mut Block<false>,
    root_set: &ThreadSafeRootSet,
    force: bool,
    next_limit: &mut usize,
) {
    let bytes = block.objects.allocated_bytes();
    if cfg!(not(test)) && !force && bytes < *next_limit {
        return;
    }

    block.uninterned_symbol_map.clear();
    let mut state = GcState::new();
    for x in root_set.roots.lock().unwrap().iter() {
        // SAFETY: The contract of root structs will ensure that it removes
        // itself from this list before it drops.
        unsafe {
            (**x).trace(&mut state);
        }
    }

    state.trace_stack();

    *next_limit = (state.to_space.allocated_bytes() * Context::GC_GROWTH_FACTOR) / 10;
    block.drop_stack.borrow_mut().clear();
    // Find all hashtables that have not been moved (i.e. They are no longer
    // accessible) and drop them. Otherwise, update the object pointer.
    block.lisp_hashtables.borrow_mut().retain_mut(|ptr| {
        let table = unsafe { &**ptr };
        if let Some(fwd) = table.forwarding_ptr() {
            *ptr = fwd.as_ptr().cast::<LispHashTable>();
            true
        } else {
            unsafe { std::ptr::drop_in_place(*ptr as *mut LispHashTable) };
            false
        }
    });

    block.lisp_chartables.borrow_mut().retain_mut(|ptr| {
        let table = unsafe { &**ptr };
        if let Some(fwd) = table.forwarding_ptr() {
            *ptr = fwd.as_ptr().cast::<CharTable>();
            true
        } else {
            unsafe { std::ptr::drop_in_place(*ptr as *mut CharTable) };
            false
        }
    });

    block.objects = state.to_space;
}

impl Deref for Context<'_> {
    type Target = Block<false>;

    fn deref(&self) -> &Self::Target {
        &self.block
    }
}

impl AsRef<Block<false>> for Context<'_> {
    fn as_ref(&self) -> &Block<false> {
        &self.block
    }
}

impl<const CONST: bool> Drop for Block<CONST> {
    // Only one block can exist in a thread at a time. This part of that
    // contract.
    fn drop(&mut self) {
        SINGLETON_CHECK.with(|s| {
            assert!(s.get(), "Context singleton check was overwritten");
            s.set(false);
        });
    }
}

#[cfg(test)]
mod test {
    use rune_core::macros::{list, rebind, root};

    use crate::core::{
        cons::Cons,
        object::{HashTable, ObjectType, Symbol},
    };

    use super::*;
    fn bind_to_mut<'ob>(cx: &'ob mut Context) -> Object<'ob> {
        cx.add("invariant")
    }

    #[test]
    fn test_reborrow() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let obj = rebind!(bind_to_mut(cx));
        _ = "foo".into_obj(cx);
        assert_eq!(obj, "invariant");
    }

    #[test]
    fn test_garbage_collect() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(vec, new(Vec), cx);
        cx.garbage_collect(true);
        let cons = list!["foo", 1, false, "end"; cx];
        vec.push(cons);
        cx.garbage_collect(true);
    }

    #[test]
    fn test_move_values() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let int = cx.add(1);
        let float = cx.add(1.5);
        let cons: Object = Cons::new(int, float, cx).into();
        let string = cx.add("string");
        let symbol = cx.add(Symbol::new_uninterned("sym", cx));
        println!("sym: {:?}", symbol.into_raw());
        let mut table = HashTable::default();
        table.insert(symbol, string);
        let _ = table.get(&symbol).unwrap();
        root!(symbol, cx);
        let table = cx.add(table);
        let vec = vec![cons, table];
        let vec = cx.add(vec);
        root!(vec, cx);
        cx.garbage_collect(true);
        let vec = vec.bind(cx);
        let ObjectType::Vec(vec) = vec.untag() else { unreachable!() };
        let ObjectType::Cons(cons) = vec[0].get().untag() else { unreachable!() };
        let ObjectType::HashTable(table) = vec[1].get().untag() else { unreachable!() };
        let key = symbol.bind(cx);
        println!("key: {:?}", key.into_raw());
        let val = table.get(symbol.bind(cx)).unwrap();
        let ObjectType::String(string) = val.untag() else { unreachable!() };
        let ObjectType::Int(int) = cons.car().untag() else { unreachable!() };
        let ObjectType::Float(float) = cons.cdr().untag() else { unreachable!() };
        assert_eq!(string, "string");
        assert_eq!(**float, 1.5);
        assert_eq!(int, 1);
    }
}

#[cfg(test)]
mod domain_constructor_tests {
    use super::*;

    #[test]
    fn local_block_has_no_safe_default_constructor() {
        trait AmbiguousIfDefault<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfDefault<()> for T {}
        impl<T: ?Sized + Default> AmbiguousIfDefault<u8> for T {}
        let _ = <Block<false> as AmbiguousIfDefault<_>>::check;
    }

    #[test]
    fn duplicate_context_rejection_preserves_original_owner() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let other_roots = RootSet::default();
        let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _second = Context::new(&other_roots);
        }));
        assert!(second.is_err());
        assert_eq!(cx.add(7), 7);
        cx.garbage_collect(true);
    }
}
