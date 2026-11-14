//! Owner-thread mutation seam; UI consumes immutable state, never a second store.
use crate::{
    BufferId, Command, CommandOutcome, Dispatch, Document, Key, Result, Session, WindowId,
};
use std::ops::Range;
/// No Send bound: a runtime host borrows a thread-confined Context.
pub trait EditorHost {
    type Doc: Document;
    fn snapshot(&self) -> Result<crate::SessionSnapshot<Self::Doc>>;
    fn execute(&mut self, command: Command) -> Result<CommandOutcome>;
    fn dispatch_key(&mut self, key: Key) -> Result<Dispatch>;
    fn cancel_prefix(&mut self) -> Result<()>;
    /// Unsupported hosts fail explicitly; never manufacture a successful scroll.
    fn scroll_window(
        &mut self,
        _window: WindowId,
        _buffer: BufferId,
        _sequence: u64,
        _lines: i64,
    ) -> Result<bool> {
        Err(crate::EditorError::Backend(
            "Scoped scrolling is unsupported by this host".into(),
        ))
    }
    fn place_pointer(
        &mut self,
        window: WindowId,
        buffer: BufferId,
        epoch: u64,
        position: usize,
    ) -> Result<CommandOutcome>;
    fn commit_replacement(
        &mut self,
        window: WindowId,
        buffer: BufferId,
        epoch: u64,
        range: Range<usize>,
        text: String,
    ) -> Result<CommandOutcome>;
}
impl<D: Document> EditorHost for Session<D> {
    type Doc = D;
    fn snapshot(&self) -> Result<crate::SessionSnapshot<D>> {
        Ok(Session::snapshot(self))
    }
    fn execute(&mut self, c: Command) -> Result<CommandOutcome> {
        Session::execute(self, c)
    }
    fn dispatch_key(&mut self, k: Key) -> Result<Dispatch> {
        self.dispatch(k)
    }
    fn cancel_prefix(&mut self) -> Result<()> {
        Session::cancel_prefix(self);
        Ok(())
    }
    fn scroll_window(
        &mut self,
        w: WindowId,
        b: BufferId,
        sequence: u64,
        lines: i64,
    ) -> Result<bool> {
        Session::scroll_window(self, w, b, sequence, lines)
    }
    fn place_pointer(
        &mut self,
        w: WindowId,
        b: BufferId,
        e: u64,
        p: usize,
    ) -> Result<CommandOutcome> {
        Session::place_pointer(self, w, b, e, p)
    }
    fn commit_replacement(
        &mut self,
        w: WindowId,
        b: BufferId,
        e: u64,
        r: Range<usize>,
        t: String,
    ) -> Result<CommandOutcome> {
        Session::commit_replacement(self, w, b, e, r, t)
    }
}
