use super::{CloneIn, Gc, IntoObject, NIL, ObjCell, Object, ObjectType, TagType};
use crate::core::gc::{AllocState, Block, Context, GcHeap, GcMoveable, GcState, Rto, Trace};
use anyhow::{Result, ensure};
use std::cell::Cell;
use std::fmt::{Debug, Display};
use std::ops::Deref;
use std::ptr::NonNull;

pub(crate) type GcString<'a> = bumpalo::collections::String<'a>;
pub(crate) struct LispString(GcHeap<LispStringInner>);

// This type needs to be this complex due to to string mutation.
//
// Case 1: The new char is the same utf8 size as the old one:
// We can update the string in place using the mutable pointer.
//
// Case 2: The new char is a different size:
// Need to allocate a new string and update the cell to point to that.
struct LispStringInner {
    bytes: Cell<*mut str>,
    properties: ObjCell,
}

impl GcMoveable for LispString {
    type Value = std::ptr::NonNull<LispString>;

    fn move_value(&self, to_space: &bumpalo::Bump) -> Option<(Self::Value, bool)> {
        match self.0.allocation_state() {
            AllocState::Forwarded(f) => Some((f.cast::<Self>(), false)),
            AllocState::Global => None,
            AllocState::Unmoved => {
                let ptr = {
                    let mut new = GcString::from_str_in(self, to_space);
                    let lisp_str = unsafe {
                        LispString::with_properties(new.as_mut_str(), false, self.property_root())
                    };
                    std::mem::forget(new);
                    let alloc = to_space.alloc(lisp_str);
                    NonNull::from(alloc)
                };
                self.0.forward(ptr.cast::<u8>());
                Some((ptr, true))
            }
        }
    }
}

impl Trace for LispString {
    fn trace(&self, state: &mut GcState) {
        self.0.properties.trace(state);
    }
}

impl Debug for LispString {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        Debug::fmt(self.inner(), f)
    }
}

impl Display for LispString {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut output = String::new();
        for c in self.inner().chars() {
            match c {
                '\\' => output.push_str("\\\\"),
                '"' => output.push_str("\\\""),
                c => output.push(c),
            }
        }
        Display::fmt(&output, f)
    }
}

impl PartialEq for LispString {
    fn eq(&self, other: &Self) -> bool {
        self.inner() == other.inner()
    }
}

impl Eq for LispString {}

impl PartialEq<str> for LispString {
    fn eq(&self, other: &str) -> bool {
        self.inner() == other
    }
}

impl Deref for LispString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.inner()
    }
}

impl LispString {
    pub(in crate::core) unsafe fn new(string: *mut str, constant: bool) -> Self {
        unsafe { Self::with_properties(string, constant, NIL) }
    }

    unsafe fn with_properties(string: *mut str, constant: bool, properties: Object) -> Self {
        Self(GcHeap::new(
            LispStringInner {
                bytes: Cell::new(string),
                properties: unsafe { ObjCell::new(properties) },
            },
            constant,
        ))
    }
    pub(crate) fn property_root(&self) -> Object<'_> {
        self.0.properties.get()
    }
    pub(crate) fn has_properties(&self) -> bool {
        !self.property_root().is_nil()
    }
    pub(crate) fn property_runs(&self) -> Result<Vec<StringPropertyRun<'_>>> {
        decode_runs(self.property_root(), self.len())
    }
    /// SAFETY: self and every reachable run value belong to cx or audited globals.
    /// No callback/GC may intervene before this immediate owner publication.
    pub(crate) unsafe fn set_property_runs(
        &self,
        runs: &[StringPropertyRun<'_>],
        cx: &Context,
    ) -> Result<()> {
        ensure!(
            matches!(self.0.allocation_state(), AllocState::Unmoved),
            "Immutable or forwarded string properties"
        );
        let root = encode_runs(runs, self.len(), cx)?;
        unsafe {
            self.0.properties.as_mut().set(root);
        }
        Ok(())
    }
    pub(crate) fn inner(&self) -> &str {
        unsafe { &*self.0.bytes.get() }
    }
}

impl LispString {
    pub(crate) fn len(&self) -> usize {
        self.chars().count()
    }

    /// Physically erase a local string through an exclusive heap borrow.
    /// A rooted input avoids retaining a raw string borrow before taking cx.
    /// No Lisp allocation, callbacks, or collection occurs during this operation.
    pub(crate) fn clear_rooted(string: &Rto<Gc<&Self>>, cx: &mut Context) -> anyhow::Result<()> {
        let string = string.bind(cx).untag();
        if !matches!(string.0.allocation_state(), AllocState::Unmoved) {
            anyhow::bail!("Attempt to clear an immutable or forwarded string");
        }
        // SAFETY: The exclusive Context borrow excludes live safe borrows into
        // this local heap. Global strings are rejected above, and the root is
        // kept alive with no GC/callback here. Its backing allocation may be
        // mutated without aliasing a live &str. Volatile writes preserve actual
        // erasure rather than switching to a fresh zero-filled allocation.
        unsafe {
            let raw = string.0.bytes.get();
            let len = (&*raw).len();
            let bytes = raw.cast::<u8>();
            for offset in 0..len {
                bytes.add(offset).write_volatile(0);
            }
        }
        Ok(())
    }
}

impl<'new> CloneIn<'new, &'new Self> for LispString {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> anyhow::Result<super::Gc<&'new Self>> {
        let properties =
            if self.has_properties() { self.property_root().clone_in(bk)? } else { NIL };
        let mut bytes = GcString::from_str_in(self.inner(), &bk.objects);
        // SAFETY: bytes and recursively copied property graph belong to bk.
        let value = unsafe { Self::with_properties(bytes.as_mut_str(), C, properties) };
        std::mem::forget(bytes);
        let allocated: &Self = bk.objects.alloc(value);
        Ok(allocated.tag())
    }
}

impl AsRef<str> for LispString {
    fn as_ref(&self) -> &str {
        self
    }
}

impl<'a> From<&'a LispString> for &'a str {
    fn from(value: &'a LispString) -> Self {
        value
    }
}

impl<'a> From<&'a LispString> for &'a [u8] {
    fn from(value: &'a LispString) -> Self {
        value.as_bytes()
    }
}

struct ByteStringInner {
    bytes: *mut [u8],
    properties: ObjCell,
}
pub(crate) struct ByteString(GcHeap<ByteStringInner>);
type ByteVec<'a> = bumpalo::collections::Vec<'a, u8>;

impl Deref for ByteString {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.inner()
    }
}

impl GcMoveable for ByteString {
    type Value = std::ptr::NonNull<ByteString>;

    fn move_value(&self, to_space: &bumpalo::Bump) -> Option<(Self::Value, bool)> {
        match self.0.allocation_state() {
            AllocState::Forwarded(f) => Some((f.cast::<Self>(), false)),
            AllocState::Global => None,
            AllocState::Unmoved => {
                let ptr = {
                    let mut new = ByteVec::new_in(to_space);
                    new.extend_from_slice(self.inner());
                    let byte_string = unsafe {
                        ByteString::with_properties(new.as_mut_slice(), false, self.property_root())
                    };
                    std::mem::forget(new);
                    let alloc = to_space.alloc(byte_string);
                    NonNull::from(alloc)
                };
                self.0.forward(ptr.cast::<u8>());
                Some((ptr, true))
            }
        }
    }
}

impl PartialEq for ByteString {
    fn eq(&self, other: &Self) -> bool {
        self.inner() == other.inner()
    }
}

impl Eq for ByteString {}

impl Trace for ByteString {
    fn trace(&self, state: &mut GcState) {
        self.0.properties.trace(state);
    }
}

impl ByteString {
    pub(in crate::core) fn new(string: *mut [u8], constant: bool) -> Self {
        unsafe { Self::with_properties(string, constant, NIL) }
    }

    unsafe fn with_properties(bytes: *mut [u8], constant: bool, properties: Object) -> Self {
        Self(GcHeap::new(
            ByteStringInner { bytes, properties: unsafe { ObjCell::new(properties) } },
            constant,
        ))
    }
    pub(crate) fn property_root(&self) -> Object<'_> {
        self.0.properties.get()
    }
    pub(crate) fn has_properties(&self) -> bool {
        !self.property_root().is_nil()
    }
    pub(crate) fn property_runs(&self) -> Result<Vec<StringPropertyRun<'_>>> {
        decode_runs(self.property_root(), self.inner().len())
    }
    /// SAFETY: self and every reachable run value belong to cx or audited globals.
    pub(crate) unsafe fn set_property_runs(
        &self,
        runs: &[StringPropertyRun<'_>],
        cx: &Context,
    ) -> Result<()> {
        ensure!(
            matches!(self.0.allocation_state(), AllocState::Unmoved),
            "Immutable or forwarded byte-string properties"
        );
        let root = encode_runs(runs, self.inner().len(), cx)?;
        unsafe {
            self.0.properties.as_mut().set(root);
        }
        Ok(())
    }
    pub(crate) fn inner(&self) -> &[u8] {
        unsafe { &*self.0.bytes }
    }
}

impl<'new> CloneIn<'new, &'new Self> for ByteString {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> anyhow::Result<super::Gc<&'new Self>> {
        let properties =
            if self.has_properties() { self.property_root().clone_in(bk)? } else { NIL };
        let mut bytes = ByteVec::new_in(&bk.objects);
        bytes.extend_from_slice(self.inner());
        // SAFETY: bytes and recursively copied properties belong to target bk.
        let value = unsafe { Self::with_properties(bytes.as_mut_slice(), C, properties) };
        std::mem::forget(bytes);
        let allocated: &Self = bk.objects.alloc(value);
        Ok(allocated.tag())
    }
}

impl Display for ByteString {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for byte in &**self {
            if byte.is_ascii() {
                write!(f, "{}", *byte as char)?;
            } else {
                write!(f, "\\{byte:03o}")?;
            }
        }
        Ok(())
    }
}

impl Debug for ByteString {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

#[cfg(test)]
mod test {
    use crate::core::gc::{Context, RootSet};
    use rune_core::macros::root;

    #[test]
    fn test_string_aliasing() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let s1 = cx.add(String::from("hello"));
        let mut s2 = cx.string_with_capacity(5);
        s2.push_str("hello");
        let s2 = cx.add(s2);
        assert_eq!(s1, s2);
        root!(s1, cx);
        root!(s2, cx);
        cx.garbage_collect(true);
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_byte_string_aliasing() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let s1 = cx.add(vec![1u8, 2, 3]);
        let s2 = cx.add(vec![1u8, 2, 3]);
        let s2 = cx.add(s2);
        assert_eq!(s1, s2);
        root!(s1, cx);
        root!(s2, cx);
        cx.garbage_collect(true);
        assert_eq!(s1, s2);
    }
}

#[cfg(test)]
mod clear_ownership_tests {
    use super::*;
    use crate::core::{env::INTERNED_SYMBOLS, gc::RootSet};
    use rune_core::macros::root;

    #[test]
    fn rooted_clear_erases_original_local_allocation_without_gc() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let string = "sensitive λ".into_obj(&cx.block);
        root!(string, cx);
        let before = string.bind(cx).untag().inner().as_ptr() as usize;
        let bytes = string.bind(cx).untag().inner().len();
        LispString::clear_rooted(string, cx).unwrap();
        let cleared = string.bind(cx).untag().inner();
        assert_eq!(cleared.as_ptr() as usize, before);
        assert_eq!(cleared.as_bytes(), vec![0; bytes]);
    }

    #[test]
    fn rooted_clear_after_gc_is_visible_through_all_lisp_aliases() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let first = "shared secret".into_obj(&cx.block);
        root!(first, cx);
        let second = first.bind(cx);
        root!(second, cx);
        cx.garbage_collect(true);
        let before = first.bind(cx).untag().inner().as_ptr() as usize;
        LispString::clear_rooted(first, cx).unwrap();
        let a = first.bind(cx).untag().inner();
        let b = second.bind(cx).untag().inner();
        assert_eq!(a.as_ptr() as usize, before);
        assert_eq!(a.as_ptr(), b.as_ptr());
        assert!(a.bytes().all(|byte| byte == 0));
        assert_eq!(a, b);
    }

    #[test]
    fn rooted_clear_rejects_global_readonly_string_without_mutation() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let map = INTERNED_SYMBOLS.lock().unwrap();
        let string = "global readonly".into_obj(map.global_block());
        root!(string, cx);
        assert!(LispString::clear_rooted(string, cx).is_err());
        assert_eq!(string.bind(cx).untag().inner(), "global readonly");
    }

    #[test]
    fn borrowed_string_cannot_cross_thread_while_owner_mutates() {
        trait AmbiguousIfSend<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
        let _ = <&'static LispString as AmbiguousIfSend<_>>::check;
    }
}

/// Borrowed scalar-index runs (byte indices for ByteString). Native snapshots
/// are not roots and must not survive evaluation or a mutable Context borrow.
#[derive(Clone, Copy)]
pub(crate) struct StringPropertyRun<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) plist: Object<'a>,
}
const MAX_STRING_PROPERTY_RUNS: usize = 4096;
fn validate_runs(runs: &[StringPropertyRun<'_>], length: usize) -> Result<()> {
    ensure!(runs.len() <= MAX_STRING_PROPERTY_RUNS, "String property run limit exceeded");
    let mut previous = 0;
    for run in runs {
        ensure!(
            run.start >= previous && run.start < run.end && run.end <= length,
            "Invalid string property interval"
        );
        ensure!(run.end <= super::MAX_FIXNUM as usize, "String property index exceeds fixnum");
        previous = run.end;
    }
    Ok(())
}
fn encode_runs<'a>(
    runs: &[StringPropertyRun<'_>],
    length: usize,
    cx: &'a Context,
) -> Result<Object<'a>> {
    validate_runs(runs, length)?;
    let mut values = Vec::new();
    values.try_reserve_exact(runs.len() * 3)?;
    for run in runs {
        if !run.plist.is_nil() {
            values.extend([
                Object::from(run.start as i64),
                Object::from(run.end as i64),
                run.plist,
            ]);
        }
    }
    Ok(if values.is_empty() { NIL } else { cx.add(values) })
}
fn decode_runs(root: Object<'_>, length: usize) -> Result<Vec<StringPropertyRun<'_>>> {
    if root.is_nil() {
        return Ok(Vec::new());
    }
    let ObjectType::Vec(values) = root.untag() else {
        anyhow::bail!("Invalid string property root")
    };
    ensure!(
        values.len() % 3 == 0 && values.len() / 3 <= MAX_STRING_PROPERTY_RUNS,
        "Invalid string property run vector"
    );
    let mut runs = Vec::new();
    runs.try_reserve_exact(values.len() / 3)?;
    for row in values.chunks(3) {
        let (ObjectType::Int(start), ObjectType::Int(end)) =
            (row[0].get().untag(), row[1].get().untag())
        else {
            anyhow::bail!("Invalid string property indices")
        };
        runs.push(StringPropertyRun {
            start: usize::try_from(start)?,
            end: usize::try_from(end)?,
            plist: row[2].get(),
        });
    }
    validate_runs(&runs, length)?;
    Ok(runs)
}
pub(crate) fn string_property_runs(object: Object<'_>) -> Result<Vec<StringPropertyRun<'_>>> {
    match object.untag() {
        ObjectType::String(s) => s.property_runs(),
        ObjectType::ByteString(s) => s.property_runs(),
        _ => anyhow::bail!("Expected a string"),
    }
}
/// SAFETY: input and all provided property graphs belong to cx or audited global
/// storage, and remain rooted until immediate shared-Context publication. Values
/// are shallowly retained; caller supplies independently prepared plist spines.
pub(crate) unsafe fn copy_string_with_properties<'a>(
    object: Object<'a>,
    runs: &[StringPropertyRun<'_>],
    cx: &'a Context,
) -> Result<Object<'a>> {
    match object.untag() {
        ObjectType::String(s) => {
            validate_runs(runs, s.len())?;
            if s.is_empty() {
                return Ok(object);
            }
            let value = cx.add(s.inner().to_owned());
            let ObjectType::String(copy) = value.untag() else { unreachable!() };
            unsafe {
                copy.set_property_runs(runs, cx)?;
            }
            Ok(value)
        }
        ObjectType::ByteString(s) => {
            validate_runs(runs, s.len())?;
            if s.is_empty() {
                return Ok(object);
            }
            let value = cx.add(s.inner().to_vec());
            let ObjectType::ByteString(copy) = value.untag() else { unreachable!() };
            unsafe {
                copy.set_property_runs(runs, cx)?;
            }
            Ok(value)
        }
        _ => anyhow::bail!("Expected a string"),
    }
}

#[cfg(test)]
mod property_storage_tests {
    use super::*;
    use crate::core::{cons::Cons, env::{sym, INTERNED_SYMBOLS}, gc::RootSet, object::{make_channel_pair_for_test, ObjectType}};
    use crate::channel_manager::{ChannelManager, SendError, RecvError};
    use rune_core::macros::{root,list};
    #[test]
    fn string_and_raw_byte_properties_are_the_only_roots_through_repeated_moves() {
        for binary in [false,true] {
            let roots=RootSet::default();let cx=&mut Context::new(&roots);
            let object=if binary {cx.add(vec![0xff,0xfe])} else {cx.add("é🙂")};
            let value=Cons::new(cx.add("property-only"),NIL,cx);
            let plist=list![cx.add("key"),value;cx];
            let runs=[StringPropertyRun{start:0,end:2,plist}];
            match object.untag() {
                ObjectType::String(s)=>unsafe{s.set_property_runs(&runs,cx).unwrap()},
                ObjectType::ByteString(s)=>unsafe{s.set_property_runs(&runs,cx).unwrap()},_=>unreachable!(),
            }
            root!(object,cx);
            for _ in 0..5 {
                let old=object.bind(cx).into_raw().addr();cx.garbage_collect(true);
                assert_ne!(object.bind(cx).into_raw().addr(),old);
                let runs=string_property_runs(object.bind(cx)).unwrap();assert_eq!((runs[0].start,runs[0].end),(0,2));
                let pair=runs[0].plist.cons().unwrap().cdr().cons().unwrap().car().cons().unwrap();
                assert_eq!(<&str>::try_from(pair.car()).unwrap(),"property-only");
                if binary {let ObjectType::ByteString(bytes)=object.bind(cx).untag() else {unreachable!()};assert_eq!(bytes.inner(),&[0xff,0xfe]);}
            }
        }
    }
    #[test]
    fn property_cycles_move_collect_and_copy_returns_error_instead_of_recursing() {
        let roots=RootSet::default();let cx=&mut Context::new(&roots);sym::init_symbols();
        {
            let object=cx.add("x");let value=Cons::new(NIL,NIL,cx);let plist=list![sym::FACE,value;cx];
            let ObjectType::String(string)=object.untag() else {unreachable!()};
            unsafe{string.set_property_runs(&[StringPropertyRun{start:0,end:1,plist}],cx).unwrap();}
            value.set_car(object).unwrap();
            root!(object,cx);
            for _ in 0..5 {cx.garbage_collect(true);let runs=string_property_runs(object.bind(cx)).unwrap();let value=runs[0].plist.cons().unwrap().cdr().cons().unwrap().car().cons().unwrap();assert!(value.car().ptr_eq(object.bind(cx)));}
            let manager=ChannelManager::isolated_for_test();let(_,sender,receiver)=make_channel_pair_for_test(manager.clone(),1);
            assert_eq!(sender.try_send(object.bind(cx)),Err(SendError::CopyFailed));
            assert!(matches!(receiver.try_recv(cx),Err(RecvError::Empty)));sender.close();receiver.close();
            let function:super::super::Function=list![sym::LAMBDA,NIL,object.bind(cx);cx].try_into().unwrap();
            let symbol=crate::core::env::intern("rune-string-props-cycle",cx);
            assert!(crate::data::fset(symbol,function.into()).is_err());
        }
        cx.garbage_collect(true);assert_eq!(cx.block.objects.allocated_bytes(),0);
    }
    #[test]
    fn invalid_runs_and_global_mutation_reject_before_property_publication() {
        let roots=RootSet::default();let cx=&Context::new(&roots);sym::init_symbols();
        let object=cx.add("λx");let ObjectType::String(string)=object.untag() else {unreachable!()};
        let plist=list![sym::FACE,42;cx];unsafe{string.set_property_runs(&[StringPropertyRun{start:0,end:1,plist}],cx).unwrap();}
        let old=string.property_root();
        assert!(unsafe{string.set_property_runs(&[StringPropertyRun{start:1,end:3,plist}],cx)}.is_err());assert!(old.ptr_eq(string.property_root()));
        let mut deferred=crate::core::gc::DeferredEndpointFinalizers::default();
        let symbols=INTERNED_SYMBOLS.lock().unwrap();let block=symbols.global_block();
        let transaction=unsafe{block.copy_transaction(&mut deferred)};let _session=block.clone_session();
        let copied=object.clone_in(block).unwrap();transaction.commit();
        let ObjectType::String(global)=copied.untag() else {unreachable!()};
        assert!(unsafe{global.set_property_runs(&[],cx)}.is_err());assert_eq!(global.property_runs().unwrap().len(),1);
    }
    #[test]
    fn intrinsic_copy_budget_failure_resets_walk_and_unpublished_transaction() {
        let roots=RootSet::default();let cx=&Context::new(&roots);
        let mut graph=NIL;for _ in 0..300 {graph=Cons::new(1,graph,cx).into();}
        let manager=ChannelManager::isolated_for_test();let(_,sender,receiver)=make_channel_pair_for_test(manager.clone(),1);
        assert_eq!(sender.try_send(graph),Err(SendError::CopyFailed));
        sender.try_send(cx.add(42)).unwrap();assert_eq!(receiver.try_recv(cx).unwrap(),cx.add(42));
        sender.close();receiver.close();
    }
}
