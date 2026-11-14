use super::{CloneIn, Gc, IntoObject};
use crate::core::gc::{AllocState, Block, Context, GcHeap, GcMoveable, GcState, Rto, Trace};
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
struct LispStringInner(Cell<*mut str>);

impl GcMoveable for LispString {
    type Value = std::ptr::NonNull<LispString>;

    fn move_value(&self, to_space: &bumpalo::Bump) -> Option<(Self::Value, bool)> {
        match self.0.allocation_state() {
            AllocState::Forwarded(f) => Some((f.cast::<Self>(), false)),
            AllocState::Global => None,
            AllocState::Unmoved => {
                let ptr = {
                    let mut new = GcString::from_str_in(self, to_space);
                    let lisp_str = unsafe { LispString::new(new.as_mut_str(), false) };
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
    fn trace(&self, _state: &mut GcState) {}
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
        Self(GcHeap::new(LispStringInner(Cell::new(string)), constant))
    }

    pub(crate) fn inner(&self) -> &str {
        unsafe { &*self.0.0.get() }
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
            let raw = string.0.0.get();
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
        Ok(GcString::from_str_in(self.inner(), &bk.objects).into_obj(bk))
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

pub(crate) struct ByteString(GcHeap<*mut [u8]>);
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
                    let byte_string = ByteString::new(new.as_mut_slice(), false);
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
    fn trace(&self, _state: &mut GcState) {}
}

impl ByteString {
    pub(in crate::core) fn new(string: *mut [u8], constant: bool) -> Self {
        Self(GcHeap::new(string, constant))
    }

    pub(crate) fn inner(&self) -> &[u8] {
        unsafe { &**self.0 }
    }
}

impl<'new> CloneIn<'new, &'new Self> for ByteString {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> anyhow::Result<super::Gc<&'new Self>> {
        Ok(
            // TODO: Allocate in the non-gc heap here
            (**self).to_vec().into_obj(bk),
        )
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
