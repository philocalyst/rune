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

/// Indexed Unicode-scalar / UTF-16 boundary conversion for native IME opt-in.
/// Invalid positions (including surrogate midpoints) reject without rounding.
pub trait PlatformTextDocument: DisplayDocument {
    fn char_to_utf16(&self, scalar: usize) -> crate::Result<usize>;
    fn utf16_to_char(&self, utf16: usize) -> crate::Result<usize>;
}

/// Plain owner-computed style identity, independent from text/input epochs.
/// Error states are data so a malformed style never prevents corrective Lisp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayStyleRevision {
    pub epoch: u64,
    pub frame_id: u64,
    pub default_face: std::result::Result<rune_display::Face, &'static str>,
}
impl Default for DisplayStyleRevision {
    fn default() -> Self { Self { epoch: 0, frame_id: 0, default_face: Ok(rune_display::Face::default()) } }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowStyle {
    pub default_face: rune_display::Face,
    pub spans: Vec<rune_display::FaceSpan>,
}
