use super::{BufferProperties, Gc, Object, ObjectType, TagType, WithLifetime};
use crate::{
    core::{
        error::{Type, TypeError},
        gc::{Block, Context, GcHeap, GcState, Trace},
    },
    derive_GcMoveable,
};
use anyhow::{Result, bail};
use rune_macros::Trace;
use std::{
    fmt::Display,
    ops::{Deref, DerefMut},
    sync::{Mutex, MutexGuard, TryLockError},
};
use text_buffer::Buffer as TextBuffer;

/// A Handle to an open buffer. Only one thread can hold this at a time.
#[derive(Debug)]
pub(crate) struct OpenBuffer<'a> {
    data: MutexGuard<'a, Option<BufferData>>,
    back_ref: &'a LispBuffer,
}

impl Trace for OpenBuffer<'_> {
    fn trace(&self, state: &mut GcState) {
        // Globally stable text storage; Lisp metadata roots are owned by the
        // Context registry, not this lock (including killed cached guards).
        let _ = state;
    }
}

impl OpenBuffer<'_> {
    /// Inspect liveness through this already-held text guard, without locking
    /// the buffer again. A cached guard can remain after kill().
    pub(crate) fn is_live(&self) -> bool {
        self.data.is_some()
    }

    pub(crate) fn get(&self) -> &BufferData {
        // buffer can never be none because we check it as part of `lock`.
        self.data.as_ref().unwrap()
    }

    pub(crate) fn get_mut(&mut self) -> &mut BufferData {
        // buffer can never be none because we check it as part of `lock`.
        self.data.as_mut().unwrap()
    }

    // TODO: we shouldn't leave it empty
    pub(crate) fn kill(&mut self) -> bool {
        let killed = self.data.is_some();
        *self.data = None;
        killed
    }

    pub(crate) fn lisp_buffer<'ob>(&self, cx: &'ob Context) -> &'ob LispBuffer {
        cx.bind(self.back_ref)
    }

    pub(crate) fn insert(&mut self, arg: Object, cx: &Context) -> Result<()> {
        let metadata = self.get().textprops.clone();
        let mut properties = metadata.lock(cx)?;
        let position = self.get().text.cursor().chars() + 1;
        let old_len = self.get().text.len_chars();
        match arg.untag() {
            ObjectType::Int(i) => {
                let Ok(u_32) = i.try_into() else { bail!("{i} is an invalid char") };
                let Some(chr) = char::from_u32(u_32) else { bail!("{i} is an Invalid char") };
                self.get_mut().text.insert_char(chr);
            }
            ObjectType::String(s) => self.get_mut().text.insert(s),
            x => bail!(TypeError::new(Type::String, x)),
        }
        let chars = self.get().text.len_chars() - old_len;
        properties.insert_text(position, chars);
        Ok(())
    }

    pub(crate) fn slice_with_gap(&self, beg: usize, end: usize) -> Result<(&str, &str)> {
        let beg = self.in_range(beg)?;
        let end = self.in_range(end)?;
        Ok(self.get().text.slice(beg..end))
    }

    pub(crate) fn delete(&mut self, beg: usize, end: usize, cx: &Context) -> Result<()> {
        let metadata = self.get().textprops.clone();
        let mut properties = metadata.lock(cx)?;
        let beg = self.in_range(beg)?;
        let end = self.in_range(end)?;
        let (beg, end) = (beg.min(end), beg.max(end));
        self.get_mut().text.delete_range(beg, end);
        properties.delete_text(beg + 1, end + 1);
        Ok(())
    }

    /// Replace a character range while holding the heap-domain metadata guard.
    /// No mutation occurs if the metadata owner is foreign or has ended.
    pub(crate) fn replace_text(
        &mut self,
        start: usize,
        end: usize,
        replacement: &str,
        cx: &Context,
    ) -> Result<()> {
        let metadata = self.get().textprops.clone();
        let mut properties = metadata.lock(cx)?;
        if start > end || end > self.get().text.len_chars() {
            bail!("Invalid replacement range");
        }
        self.get_mut().text.delete_range(start, end);
        self.get_mut().text.set_cursor(start);
        self.get_mut().text.insert(replacement);
        let chars = replacement.chars().count();
        // A same-width substitution preserves the properties of every position.
        // Expansions use the same coordinate semantics as deletion + insertion.
        if chars != end - start {
            properties.delete_text(start + 1, end + 1);
            properties.insert_text(start + 1, chars);
        }
        Ok(())
    }

    fn in_range(&self, pos: usize) -> Result<usize> {
        if pos == 0 || pos > self.get().text.len_chars() + 1 {
            bail!("Position {pos} out of range in {}", self.get().name);
        }
        Ok(pos - 1)
    }
}

impl<'new> WithLifetime<'new> for OpenBuffer<'_> {
    type Out = OpenBuffer<'new>;

    unsafe fn with_lifetime(self) -> Self::Out {
        std::mem::transmute(self)
    }
}

impl PartialEq<str> for OpenBuffer<'_> {
    fn eq(&self, other: &str) -> bool {
        self.get().text == other
    }
}

impl Deref for OpenBuffer<'_> {
    type Target = BufferData;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl DerefMut for OpenBuffer<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.get_mut()
    }
}

/// The actual data of the buffer. Buffer local variables will be stored here
/// eventually.
#[derive(Debug)]
pub(crate) struct BufferData {
    pub(crate) name: String,
    pub(crate) text: TextBuffer,
    pub(crate) textprops: BufferProperties,
}

#[derive(Debug)]
struct LispBufferInner {
    text_buffer: Mutex<Option<BufferData>>,
}

/// A lisp handle to a buffer. This is a just a reference type and does not give
/// access to the contents until it is locked and a `OpenBuffer` is returned.
#[derive(PartialEq, Eq, Trace)]
pub(crate) struct LispBuffer(GcHeap<LispBufferInner>);

// SAFETY: LispBuffer is allocated only in the global nonmoving block. Text is
// mutex protected, and all moving property slots use Context ownership guards.
unsafe impl Sync for LispBuffer {}

derive_GcMoveable!(LispBuffer);

impl LispBuffer {
    pub(crate) fn create(name: String, block: &Block<true>) -> &LispBuffer {
        let buffer = unsafe { Self::new(name, block) };
        block.objects.alloc(buffer)
    }

    pub(crate) unsafe fn new(name: String, _: &Block<true>) -> LispBuffer {
        let textprops = BufferProperties::new();
        let new = LispBufferInner {
            text_buffer: Mutex::new(Some(BufferData { name, text: TextBuffer::new(), textprops })),
        };
        Self(GcHeap::new(new, true))
    }

    pub(crate) fn lock(&self) -> Result<OpenBuffer<'_>> {
        let guard = self.0.text_buffer.lock().unwrap();
        if guard.is_none() {
            bail!("selecting deleted buffer");
        }
        Ok(OpenBuffer { data: guard, back_ref: self })
    }
}

impl PartialEq for LispBufferInner {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl PartialEq<OpenBuffer<'_>> for LispBuffer {
    fn eq(&self, other: &OpenBuffer) -> bool {
        other.back_ref == self
    }
}

impl PartialEq<LispBuffer> for OpenBuffer<'_> {
    fn eq(&self, other: &LispBuffer) -> bool {
        self.back_ref == other
    }
}

impl Eq for LispBufferInner {}

impl Display for LispBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // Diagnostics may format a property referring to a buffer whose text
        // guard is already held by this operation. Never reenter that mutex.
        let data = match self.0.text_buffer.try_lock() {
            Ok(data) => data,
            Err(TryLockError::WouldBlock) => return f.write_str("#<locked buffer>"),
            Err(TryLockError::Poisoned(_)) => return f.write_str("#<poisoned buffer>"),
        };
        let name = match data.as_ref() {
            Some(buf) => &buf.name,
            None => "deleted buffer",
        };
        write!(f, "#<{name}>")
    }
}

impl std::fmt::Debug for LispBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl Trace for LispBufferInner {
    fn trace(&self, _: &mut GcState) {
        // The globally allocated handle and text contain no moving Lisp values.
        // Metadata is traced by Context's registry. In particular, never acquire
        // the text mutex here: a rooted cached OpenBuffer may already hold it.
    }
}

impl<'new> LispBuffer {
    pub(in crate::core) fn clone_in<const C: bool>(
        &self,
        _: &'new Block<C>,
    ) -> Gc<&'new LispBuffer> {
        unsafe { self.with_lifetime().tag() }
    }
}

#[cfg(test)]
mod roadmap_m0_buffer_property_regressions {
    use super::*;
    use crate::RootSet;

    #[test]
    fn diagnostics_do_not_reenter_held_text_mutex() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let name = format!("diagnostic-lock-{}", cx.domain_id());
        let object = crate::buffer::get_buffer_create(cx.add(name), None, &cx)?;
        let handle = object.buffer().unwrap();
        let _buffer = handle.lock()?;
        assert_eq!(format!("{handle:?}"), "#<locked buffer>");
        Ok(())
    }

    #[test]
    fn replacement_inserts_at_range_start_not_unrelated_cursor() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let name = format!("replacement-cursor-{}", cx.domain_id());
        let object = crate::buffer::get_buffer_create(cx.add(name), None, &cx)?;
        let mut buffer = object.buffer().unwrap().lock()?;
        buffer.insert(cx.add("abcdef"), &cx)?;
        buffer.text.set_cursor(6);
        buffer.replace_text(1, 3, "XY", &cx)?;
        assert_eq!(buffer.text, "aXYdef");
        buffer.text.set_cursor(0);
        buffer.replace_text(3, 5, "!", &cx)?;
        assert_eq!(buffer.text, "aXY!f");
        Ok(())
    }

    #[test]
    fn roadmap_m0_buffer_edits_use_character_property_positions() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let object = crate::buffer::get_buffer_create(cx.add("roadmap-m0-properties"), None, &cx)?;
        let buffer = object.buffer().unwrap();
        let mut buffer = buffer.lock()?;
        buffer.insert(cx.add("aé🙂z"), &cx)?;
        // Lisp character positions are one-based, regardless of UTF-8 width.
        buffer.textprops.lock(&cx).unwrap().set_properties(2, 4, cx.add(7));
        buffer.text.set_cursor(0);
        buffer.insert(cx.add("λ"), &cx)?;
        assert!(buffer.textprops.lock(&cx).unwrap().find(2).is_none());
        assert_eq!(
            buffer.textprops.lock(&cx).unwrap().find(3).map(|node| *node.val),
            Some(cx.add(7))
        );
        // Reversed endpoints are accepted and normalized before editing.
        buffer.delete(4, 2, &cx)?;
        assert_eq!(
            buffer.textprops.lock(&cx).unwrap().find(2).map(|node| *node.val),
            Some(cx.add(7))
        );
        assert!(buffer.textprops.lock(&cx).unwrap().find(3).is_none());
        Ok(())
    }
}
