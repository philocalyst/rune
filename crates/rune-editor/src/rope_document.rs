use crate::Document;
use rune_text::{CharPos, PropSetId, Snapshot, TextError};
use std::ops::Range;

/// The sole authoritative text/property/marker root for an editor buffer.
/// Runtime adapters must retain the owning property-domain registry for all
/// snapshots, including undo roots. No Lisp pointers are stored here.
#[derive(Clone, Debug)]
pub struct RopeDocument {
    snapshot: Snapshot,
    insertion_properties: PropSetId,
}
impl RopeDocument {
    pub fn new(snapshot: Snapshot, insertion_properties: PropSetId) -> Self {
        Self { snapshot, insertion_properties }
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// Explicit native insertion policy, not an approximation of GNU stickiness.
    pub fn with_insertion_properties(mut self, properties: PropSetId) -> Self {
        self.insertion_properties = properties;
        self
    }
}
impl Document for RopeDocument {
    type Error = TextError;
    fn empty() -> Self {
        Self::new(Snapshot::new(rune_text::Rope::from("")), PropSetId(0))
    }
    fn len_chars(&self) -> usize {
        self.snapshot.len_chars()
    }
    fn replace(&self, range: Range<usize>, text: &str) -> Result<Self, Self::Error> {
        let (snapshot, _) = self.snapshot.edit(
            CharPos(range.start)..CharPos(range.end),
            text,
            self.insertion_properties,
        )?;
        Ok(Self { snapshot, insertion_properties: self.insertion_properties })
    }
    fn visit_chars(&self, start: usize, max: usize, visitor: &mut dyn FnMut(usize, char) -> bool) {
        if let Ok(chars) = self.snapshot.chars_from(CharPos(start)) {
            for (offset, ch) in chars.take(max).enumerate() {
                if !visitor(start + offset, ch) {
                    break;
                }
            }
        }
    }
}
#[cfg(feature = "display")]
#[derive(Clone, Debug)]
pub struct PublishedRopeDocument {
    document: RopeDocument,
    revision: u64,
}
#[cfg(feature = "display")]
impl std::ops::Deref for PublishedRopeDocument {
    type Target = RopeDocument;
    fn deref(&self) -> &Self::Target { &self.document }
}
#[cfg(feature = "display")]
impl crate::BufferSnapshot<RopeDocument> {
    /// Use the editor's branch-safe revision, not the restored text root's
    /// historical version, when keying renderer caches.
    pub fn display_snapshot(&self) -> PublishedRopeDocument {
        PublishedRopeDocument { document: self.document.clone(), revision: self.version }
    }
}
#[cfg(feature = "display")]
impl rune_display::TextSnapshot for PublishedRopeDocument {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_chars(&self) -> usize {
        self.snapshot.len_chars()
    }
    fn chars_from(&self, start: usize) -> Box<dyn Iterator<Item = char> + '_> {
        Box::new(
            self.snapshot
                .chars_from(CharPos(start.min(self.snapshot.len_chars())))
                .expect("clamped position"),
        )
    }
    fn next_line_start(&self, start: usize) -> Option<usize> {
        self.snapshot.next_line_start(CharPos(start)).map(|position| position.0)
    }
}
