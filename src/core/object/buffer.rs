//! Stable global buffer identities. All mutable text/properties/markers live in
//! the Context-owned editor Session, never in this shell or an Env cache.
use super::{Gc, TagType, WithLifetime};
use crate::{
    core::{
        env::INTERNED_SYMBOLS,
        gc::{Block, Context, GcHeap, GcState, Trace},
    },
    derive_GcMoveable,
    editor_document::{BufferRead, OwnerBufferHandle},
};
use anyhow::Result;
use rune_core::hashmap::HashMap;
use std::{
    fmt,
    sync::{LazyLock, Mutex},
};
pub(crate) type OpenBuffer = BufferRead;
pub(crate) type BufferData = BufferRead;
static SHELLS: LazyLock<Mutex<HashMap<OwnerBufferHandle, &'static LispBuffer>>> =
    LazyLock::new(|| Mutex::new(HashMap::default()));
pub(crate) struct LispBuffer(GcHeap<OwnerBufferHandle>);
// SAFETY: all shells are constant global allocations and contain only native
// checked IDs. No Context/Rc, moving Lisp value, text root, or owner borrow.
unsafe impl Sync for LispBuffer {}
impl Trace for LispBuffer {
    fn trace(&self, _: &mut GcState) {}
}
impl PartialEq for LispBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.handle() == other.handle()
    }
}
impl Eq for LispBuffer {}
derive_GcMoveable!(LispBuffer);
impl LispBuffer {
    pub(crate) fn create(handle: OwnerBufferHandle, block: &Block<true>) -> &Self {
        block.objects.alloc(Self(GcHeap::new(handle, true)))
    }
    pub(crate) fn handle(&self) -> OwnerBufferHandle {
        *self.0
    }
    pub(crate) fn for_handle<'ob>(
        handle: OwnerBufferHandle,
        cx: &'ob Context,
    ) -> Result<&'ob Self> {
        cx.editor_gateway()?.read(handle, cx)?;
        let mut shells = SHELLS.lock().unwrap();
        let shell = *shells.entry(handle).or_insert_with(|| {
            let symbols = INTERNED_SYMBOLS.lock().unwrap();
            // Global block shells never move or contain heap-local data.
            let shell = symbols.create_buffer(handle);
            unsafe { &*(shell as *const Self) }
        });
        Ok(cx.bind(shell))
    }
    pub(crate) fn read(&self, cx: &Context) -> Result<BufferRead> {
        cx.editor_gateway()?.read(self.handle(), cx)
    }
    pub(in crate::core) fn clone_in<'new, const C: bool>(
        &self,
        _: &'new Block<C>,
    ) -> anyhow::Result<Gc<&'new Self>> {
        Ok(unsafe { self.with_lifetime().tag() })
    }
}
impl fmt::Display for LispBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<buffer {:?}>", self.handle())
    }
}
impl fmt::Debug for LispBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod owner_shell_tests {
    use super::*;
    use crate::core::gc::RootSet;

    #[test]
    fn stable_shell_identity_tracks_owner_name_and_rejects_killed_handle() -> Result<()> {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let gateway = cx.editor_gateway()?;
        let handle = gateway.create("shell-exact-name", "λ🙂", cx)?;
        let shell = LispBuffer::for_handle(handle, cx)?;
        assert!(std::ptr::eq(shell, LispBuffer::for_handle(handle, cx)?));
        gateway.rename(handle, "renamed-shell", false, cx)?;
        assert_eq!(shell.read(cx)?.name, "renamed-shell");
        assert_eq!(shell.read(cx)?.document.text().to_string(), "λ🙂");
        gateway.kill(handle, true, cx)?;
        assert!(shell.read(cx).is_err());
        assert!(LispBuffer::for_handle(handle, cx).is_err());
        Ok(())
    }
}
