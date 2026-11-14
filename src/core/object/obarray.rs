//! Genuine owner-local symbol interning tables. Membership is traced, never a
//! global native side map or a global-interned flag on moving owner symbols.
use super::{CloneIn, Gc, IntoObject, ObjCell, ObjectType, Symbol, TagType, WithLifetime};
use crate::core::gc::{AllocState, Block, Context, GcHeap, GcState, OwnerLiveness, Trace};
use anyhow::{Result, ensure};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_OBARRAY: AtomicU64 = AtomicU64::new(1);
const MAX_ENTRIES: usize = 65_536;
const MAX_NAME_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub(crate) struct ObarrayInner {
    id: u64,
    owner: u64,
    owner_liveness: OwnerLiveness,
    entries: RefCell<HashMap<String, ObjCell>>,
    name_bytes: Cell<usize>,
}
pub(crate) struct LispObarray(pub(in crate::core) GcHeap<ObarrayInner>);
impl LispObarray {
    pub(crate) fn new<'a>(cx: &'a Context, capacity: usize) -> Result<Gc<&'a Self>> {
        ensure!(capacity <= MAX_ENTRIES, "Obarray capacity limit exceeded");
        let id = NEXT_OBARRAY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Obarray identities exhausted"))?;
        let mut entries = HashMap::new();
        entries.try_reserve(capacity)?;
        Ok(Self(GcHeap::new(
            ObarrayInner {
                id,
                owner: cx.domain_id(),
                owner_liveness: cx.owner_liveness(),
                entries: RefCell::new(entries),
                name_bytes: Cell::new(0),
            },
            false,
        ))
        .into_obj(&cx.block))
    }
    pub(crate) fn validate(&self, cx: &Context) -> Result<()> {
        ensure!(
            self.0.owner == cx.domain_id()
                && self.0.owner_liveness.is_live()
                && cx.block.belongs_to_owner(self.0.owner),
            "Invalid Obarray owner/lifetime"
        );
        Ok(())
    }
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }
    pub(crate) fn len(&self, cx: &Context) -> Result<usize> {
        self.validate(cx)?;
        Ok(self.0.entries.try_borrow()?.len())
    }
    fn checked_name(name: &str) -> Result<()> {
        ensure!(name.len() <= MAX_NAME_BYTES, "Obarray name byte limit exceeded");
        Ok(())
    }
    fn bound_symbol<'a>(&self, cell: &ObjCell, cx: &'a Context) -> Result<Symbol<'a>> {
        self.validate(cx)?;
        let ObjectType::Symbol(symbol) = cell.get().untag() else {
            anyhow::bail!("Invalid private Obarray symbol cell")
        };
        // SAFETY: the private map only stores symbols freshly allocated by
        // intern in this branded owner arena. Validated shared Context access
        // excludes collection; this narrows the cell's erased graph lifetime to
        // the live owner borrow, not to the temporary RefCell guard.
        Ok(unsafe { symbol.with_lifetime() })
    }
    pub(crate) fn get<'a>(&self, name: &str, cx: &'a Context) -> Result<Option<Symbol<'a>>> {
        self.validate(cx)?;
        Self::checked_name(name)?;
        self.0
            .entries
            .try_borrow()?
            .get(name)
            .map(|cell| self.bound_symbol(cell, cx))
            .transpose()
    }
    pub(crate) fn intern<'a>(&self, name: &str, cx: &'a Context) -> Result<Symbol<'a>> {
        self.validate(cx)?;
        Self::checked_name(name)?;
        let mut entries = self.0.entries.try_borrow_mut()?;
        if let Some(cell) = entries.get(name) {
            return self.bound_symbol(cell, cx);
        }
        ensure!(entries.len() < MAX_ENTRIES, "Obarray entry limit exceeded");
        let bytes = self
            .0
            .name_bytes
            .get()
            .checked_add(name.len())
            .ok_or_else(|| anyhow::anyhow!("Obarray name byte overflow"))?;
        ensure!(bytes <= MAX_TOTAL_BYTES, "Obarray total name bytes exceeded");
        entries.try_reserve(1)?;
        let mut key = String::new();
        key.try_reserve_exact(name.len())?;
        key.push_str(name);
        // These are owner-arena moving symbols, not global interned symbols.
        // Shared Context excludes callbacks/collection during fresh publication.
        let symbol = Symbol::new_uninterned(name, &cx.block);
        // SAFETY: the fresh symbol belongs to the authenticated arena, and all
        // its fields are native names or audited global function references.
        let cell = unsafe { ObjCell::new(symbol.into()) };
        entries.insert(key, cell);
        self.0.name_bytes.set(bytes);
        Ok(symbol)
    }
    pub(crate) fn remove(
        &self,
        name: &str,
        expected: Option<Symbol>,
        cx: &Context,
    ) -> Result<bool> {
        self.validate(cx)?;
        Self::checked_name(name)?;
        let mut entries = self.0.entries.try_borrow_mut()?;
        let Some(cell) = entries.get(name) else { return Ok(false) };
        let ObjectType::Symbol(symbol) = cell.get().untag() else {
            unreachable!("private Obarray symbol cell")
        };
        if expected.is_some_and(|wanted| wanted != symbol) {
            return Ok(false);
        }
        let (key, _) = entries.remove_entry(name).expect("entry checked under same borrow");
        self.0.name_bytes.set(self.0.name_bytes.get() - key.len());
        Ok(true)
    }
    /// Snapshot only under shared owner Context. Root the complete returned
    /// symbol vector BEFORE any callback/moving collection, and retain no map
    /// borrow across evaluation. Enumeration order is deliberately unspecified.
    pub(crate) fn snapshot<'a>(&self, cx: &'a Context, limit: usize) -> Result<Vec<Symbol<'a>>> {
        self.validate(cx)?;
        let entries = self.0.entries.try_borrow()?;
        ensure!(
            entries.len() <= limit && entries.len() <= MAX_ENTRIES,
            "Obarray snapshot limit exceeded"
        );
        let mut symbols = Vec::new();
        symbols.try_reserve_exact(entries.len())?;
        for cell in entries.values() {
            symbols.push(self.bound_symbol(cell, cx)?);
        }
        Ok(symbols)
    }
    pub(in crate::core) fn forwarding_ptr(&self) -> Option<std::ptr::NonNull<u8>> {
        match self.0.allocation_state() {
            AllocState::Forwarded(p) => Some(p),
            AllocState::Unmoved => None,
            AllocState::Global => unreachable!("global Obarray violates transfer invariant"),
        }
    }
}
impl Trace for ObarrayInner {
    fn trace(&self, state: &mut GcState) {
        for symbol in self.entries.borrow().values() {
            symbol.trace(state);
        }
    }
}
impl<'new> CloneIn<'new, &'new Self> for LispObarray {
    fn clone_in<const C: bool>(&self, target: &'new Block<C>) -> Result<Gc<&'new Self>> {
        ensure!(
            target.belongs_to_owner(self.0.owner) && self.0.owner_liveness.is_live(),
            "Obarray transfer across heap domains is unsupported"
        );
        // SAFETY: private owner-arena constructor and Context-only arena brand
        // establish allocation lifetime; no foreign destination passes above.
        Ok(unsafe { self.with_lifetime().tag() })
    }
}
impl PartialEq for LispObarray {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}
impl Eq for LispObarray {}
impl fmt::Debug for LispObarray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<obarray:{}>", self.id())
    }
}
impl fmt::Display for LispObarray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
crate::derive_GcMoveable!(LispObarray);

#[cfg(test)]
thread_local! {static DROPPED:Cell<usize>=const{Cell::new(0)};}
#[cfg(test)]
impl Drop for ObarrayInner {
    fn drop(&mut self) {
        DROPPED.with(|n| n.set(n.get() + 1));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        gc::RootSet,
        object::{NIL, Object},
    };
    use rune_core::macros::root;
    #[test]
    fn membership_and_names_move_with_the_real_owner_array() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let array = LispObarray::new(cx, 0).unwrap();
        let symbol = array.intern("local-λ", cx).unwrap();
        assert_eq!(symbol, array.intern("local-λ", cx).unwrap());
        assert_ne!(array.intern("nil", cx).unwrap(), crate::core::env::sym::NIL);
        assert_ne!(array.intern("t", cx).unwrap(), crate::core::env::sym::TRUE);
        let empty = array.intern("", cx).unwrap();
        assert_eq!(empty.name(), "");
        root!(array, cx);
        for _ in 0..3 {
            let old = Object::from(array.bind(cx).get("local-λ", cx).unwrap().unwrap())
                .into_raw()
                .addr();
            cx.garbage_collect(true);
            let symbol = array.bind(cx).get("local-λ", cx).unwrap().unwrap();
            assert_ne!(Object::from(symbol).into_raw().addr(), old);
            assert_eq!(symbol.name(), "local-λ");
            assert_eq!(array.bind(cx).intern("local-λ", cx).unwrap(), symbol);
        }
        assert_eq!(array.bind(cx).len(cx).unwrap(), 4);
    }
    #[test]
    fn exact_membership_removal_does_not_destroy_a_rooted_symbol() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let array = LispObarray::new(cx, 0).unwrap();
        let other = LispObarray::new(cx, 0).unwrap();
        let symbol = array.intern("same", cx).unwrap();
        let other_symbol = other.intern("same", cx).unwrap();
        assert_ne!(symbol, other_symbol);
        assert!(!array.remove("same", Some(other_symbol), cx).unwrap());
        root!(symbol, cx);
        root!(array, cx);
        assert!(array.bind(cx).remove("same", Some(symbol.bind(cx)), cx).unwrap());
        assert!(array.bind(cx).get("same", cx).unwrap().is_none());
        cx.garbage_collect(true);
        assert_eq!(symbol.bind(cx).name(), "same");
        let replacement = array.bind(cx).intern("same", cx).unwrap();
        assert_ne!(replacement, symbol.bind(cx));
    }
    #[test]
    fn native_map_finalizes_and_foreign_copy_rejects_without_shell() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let before = DROPPED.with(Cell::get);
        {
            let array = LispObarray::new(cx, 0).unwrap();
            array.intern("only-member", cx).unwrap();
            assert!(Object::from(array).ptr_eq(Object::from(array.clone_in(&cx.block).unwrap())));
            {
                let globals = crate::core::env::INTERNED_SYMBOLS.lock().unwrap();
                assert!(array.clone_in(globals.global_block()).is_err());
            }
            let (sender, receiver) = crate::core::object::make_channel_pair(1);
            assert!(matches!(
                sender.try_send(array.into()),
                Err(crate::channel_manager::SendError::CopyFailed)
            ));
            sender.close();
            receiver.close();
        }
        cx.garbage_collect(true);
        assert_eq!(DROPPED.with(Cell::get), before + 1);
        assert!(LispObarray::new(cx, MAX_ENTRIES + 1).is_err());
        let array = LispObarray::new(cx, 0).unwrap();
        assert!(array.intern(&"x".repeat(MAX_NAME_BYTES + 1), cx).is_err());
        assert_eq!(array.len(cx).unwrap(), 0);
        assert!(array.snapshot(cx, 0).unwrap().is_empty());
        let _ = NIL;
    }
    #[test]
    fn complete_snapshot_roots_survive_array_finalization_and_membership_changes() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let before = DROPPED.with(Cell::get);
        let array = LispObarray::new(cx, 0).unwrap();
        array.intern("snapshot-a", cx).unwrap();
        array.intern("snapshot-b", cx).unwrap();
        assert!(array.snapshot(cx, 1).is_err());
        root!(symbols, array.snapshot(cx, 2).unwrap(), cx);
        assert!(array.remove("snapshot-a", None, cx).unwrap());
        assert!(array.remove("snapshot-b", None, cx).unwrap());
        cx.garbage_collect(true);
        assert_eq!(DROPPED.with(Cell::get), before + 1);
        let mut names = Vec::new();
        for symbol in symbols.iter() {
            names.push(symbol.bind(cx).name().to_owned());
        }
        names.sort();
        assert_eq!(names, ["snapshot-a", "snapshot-b"]);
        cx.garbage_collect(true);
        for symbol in symbols.iter() {
            assert!(symbol.bind(cx).name().starts_with("snapshot-"));
        }
    }

    #[test]
    fn obarray_dump_rejects_before_existing_image_overwrite() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        crate::core::env::sym::init_symbols();
        root!(env, init(crate::core::env::Env::new_isolated(cx).unwrap()), cx);
        let array = LispObarray::new(cx, 0).unwrap();
        array.intern("dump-member", cx).unwrap();
        env.set_var(crate::lread::intern("rune-obarray-dump", cx), array.into())
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "rune-obarray-dump-{}-{}",
            std::process::id(),
            cx.domain_id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("existing.dump");
        std::fs::write(&path, b"existing image").unwrap();
        let result = crate::dump::dump_to_file(&path, env, cx);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(bytes, b"existing image");
    }
}
