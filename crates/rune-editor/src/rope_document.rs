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
    type MarkerUndo = rune_text::DeletionMarkers;
    fn capture_marker_undo(&self, range: Range<usize>) -> Result<Self::MarkerUndo, Self::Error> {
        rune_text::DeletionMarkers::capture(
            self.snapshot.positions(),
            CharPos(range.start)..CharPos(range.end),
        )
    }
    fn restore_history(
        &self,
        historical: &Self,
        splice: Option<crate::Splice>,
        records: Option<&Self::MarkerUndo>,
    ) -> Result<Self, Self::Error> {
        let delta = splice
            .map(|s| rune_text::EditDelta::new(CharPos(s.start)..CharPos(s.end), s.inserted))
            .transpose()?;
        let snapshot = match (delta.as_ref(), records) {
            (Some(d), Some(r)) => {
                self.snapshot.restore_content_with_marker_undo(&historical.snapshot, d, r)?
            }
            _ => self.snapshot.restore_content(&historical.snapshot, delta.as_ref())?,
        };
        Ok(Self { snapshot, insertion_properties: self.insertion_properties })
    }
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
pub type PublishedRopeDocument = crate::PublishedDocument<RopeDocument>;
#[cfg(feature = "display")]
impl crate::DisplayDocument for RopeDocument {
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
impl crate::MarkerDocument for RopeDocument {
    fn set_live_marker(&self,id:u64,position:usize,bias:crate::Bias)->Result<Self,Self::Error>{
        let bias=match bias {crate::Bias::Left=>rune_text::Bias::Left,crate::Bias::Right=>rune_text::Bias::Right};
        Ok(Self { snapshot:self.snapshot.with_marker(rune_text::MarkerId(id),CharPos(position),bias)?,insertion_properties:self.insertion_properties })
    }
    fn remove_live_marker(&self,id:u64)->Self {Self {snapshot:self.snapshot.without_marker(rune_text::MarkerId(id)),insertion_properties:self.insertion_properties}}
    fn marker_position(&self,id:u64)->Option<usize>{self.snapshot.positions().get(rune_text::MarkerId(id)).map(|(p,_)|p.0)}
}
