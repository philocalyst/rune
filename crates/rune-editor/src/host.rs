//! Owner-thread mutation seam; UI consumes immutable state, never a second store.
use crate::{
    BufferId, Command, CommandOutcome, Dispatch, Document, Key, Result, Session, WindowId,
};
use std::ops::Range;
/// No Send bound: a runtime host borrows a thread-confined Context.
pub trait EditorHost {
    type Doc: Document;
    fn session(&self) -> &Session<Self::Doc>;
    fn execute(&mut self, command: Command) -> Result<CommandOutcome>;
    fn dispatch_key(&mut self, key: Key) -> Result<Dispatch>;
    fn cancel_prefix(&mut self);
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
    fn session(&self) -> &Self {
        self
    }
    fn execute(&mut self, c: Command) -> Result<CommandOutcome> {
        Session::execute(self, c)
    }
    fn dispatch_key(&mut self, k: Key) -> Result<Dispatch> {
        self.dispatch(k)
    }
    fn cancel_prefix(&mut self) {
        Session::cancel_prefix(self)
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
