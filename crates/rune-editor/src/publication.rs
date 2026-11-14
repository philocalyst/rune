//! Immutable renderer boundary; publication revision belongs to the editor,
//! never to a historical text root restored through undo.
use crate::{BufferSnapshot, Document};

pub trait DisplayDocument: Document + Send + Sync {
    fn chars_from(&self, start: usize) -> Box<dyn Iterator<Item = char> + '_>;
    fn next_line_start(&self, start: usize) -> Option<usize>;
}
#[derive(Clone, Debug)]
pub struct PublishedDocument<D> {
    document: D,
    revision: u64,
}
impl<D> std::ops::Deref for PublishedDocument<D> {
    type Target = D;
    fn deref(&self) -> &D {
        &self.document
    }
}
impl<D: DisplayDocument> BufferSnapshot<D> {
    pub fn display_snapshot(&self) -> PublishedDocument<D> {
        PublishedDocument { document: self.document.clone(), revision: self.version }
    }
}
impl<D: DisplayDocument> rune_display::TextSnapshot for PublishedDocument<D> {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_chars(&self) -> usize {
        self.document.len_chars()
    }
    fn chars_from(&self, start: usize) -> Box<dyn Iterator<Item = char> + '_> {
        self.document.chars_from(start)
    }
    fn next_line_start(&self, start: usize) -> Option<usize> {
        self.document.next_line_start(start)
    }
}
