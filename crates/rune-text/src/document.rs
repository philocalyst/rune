//! Atomic text/property publication and typed edit coordinates. Runtime hooks,
//! undo policy and GC-owned values remain outside this pure storage crate.
use crate::{
    BytePos, CharPos, LinePos, PositionTree, PropRuns, PropSetId, Rope, TextError, TextResult,
    Version,
};
use std::ops::Range;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Left,
    Right,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditDelta {
    pub old: Range<CharPos>,
    pub inserted_chars: usize,
}
impl EditDelta {
    pub fn new(old: Range<CharPos>, inserted_chars: usize) -> TextResult<Self> {
        if old.start > old.end {
            return Err(TextError::ReversedRange);
        }
        old.start.0.checked_add(inserted_chars).ok_or(TextError::OutOfBounds)?;
        Ok(Self { old, inserted_chars })
    }
    /// Positions strictly after the replaced region translate by its length
    /// delta. Interior positions collapse with explicit insertion bias. The end
    /// of a nonempty replaced region belongs to the surviving suffix.
    pub fn map(&self, position: CharPos, bias: Bias) -> TextResult<CharPos> {
        if position < self.old.start {
            return Ok(position);
        }
        let new_end = self
            .old
            .start
            .0
            .checked_add(self.inserted_chars)
            .ok_or(TextError::OutOfBounds)?;
        if position >= self.old.end && self.old.start < self.old.end {
            return position
                .0
                .checked_sub(self.old.end.0)
                .and_then(|n| n.checked_add(new_end))
                .map(CharPos)
                .ok_or(TextError::OutOfBounds);
        }
        if position > self.old.end {
            return position
                .0
                .checked_add(self.inserted_chars)
                .map(CharPos)
                .ok_or(TextError::OutOfBounds);
        }
        Ok(match bias {
            Bias::Left => self.old.start,
            Bias::Right => CharPos(new_end),
        })
    }
}
#[derive(Clone, Debug)]
pub struct EditRecord {
    pub version: Version,
    pub delta: EditDelta,
    pub old_bytes: Range<BytePos>,
    pub new_bytes: Range<BytePos>,
    pub old_lines: Range<LinePos>,
    pub new_lines: Range<LinePos>,
}
/// Cheap, thread-safe consistent publication. Property IDs contain no Lisp
/// pointers; their owning domain must resolve them separately.
#[derive(Clone, Debug)]
pub struct Snapshot {
    text: Rope,
    props: PropRuns,
    positions: PositionTree,
    version: Version,
}
impl Snapshot {
    pub fn new(text: Rope) -> Self {
        let props = PropRuns::new(text.len_chars());
        let positions = PositionTree::new(text.len_chars());
        Self { text, props, positions, version: Version(0) }
    }
    pub fn text(&self) -> &Rope {
        &self.text
    }
    pub fn properties(&self) -> &PropRuns {
        &self.props
    }
    pub fn positions(&self) -> &PositionTree {
        &self.positions
    }
    pub fn version(&self) -> Version {
        self.version
    }
    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }
    pub fn chars_from(&self, position: CharPos) -> TextResult<crate::Chars<'_>> {
        self.text.chars_from(position)
    }
    pub fn next_line_start(&self, position: CharPos) -> Option<CharPos> {
        self.text.next_line_start(position)
    }
    /// Add or move a marker without changing the text revision. Existing
    /// snapshots retain their original marker index.
    pub fn with_marker(
        &self,
        id: crate::MarkerId,
        position: CharPos,
        bias: Bias,
    ) -> TextResult<Self> {
        let positions = self.positions.remove(id).insert(id, position, bias)?;
        Ok(Self {
            text: self.text.clone(),
            props: self.props.clone(),
            positions,
            version: self.version,
        })
    }
    pub fn without_marker(&self, id: crate::MarkerId) -> Self {
        Self {
            text: self.text.clone(),
            props: self.props.clone(),
            positions: self.positions.remove(id),
            version: self.version,
        }
    }
    /// Borrow a bounded display window. No flattened whole-buffer copy.
    pub fn chunks(&self, range: Range<CharPos>) -> TextResult<crate::Chunks<'_>> {
        if range.start > range.end {
            return Err(TextError::ReversedRange);
        }
        self.text
            .chunks(self.text.char_to_byte(range.start)?..self.text.char_to_byte(range.end)?)
    }
    /// Persistent copy/paste payload retaining the source property runs.
    /// Marker ownership stays with the document, not the copied text.
    pub fn slice(&self, range: Range<CharPos>) -> TextResult<Self> {
        let text = self.text.slice(range.clone())?;
        let props = self.props.slice(range)?;
        let positions = PositionTree::new(text.len_chars());
        Ok(Self { text, props, positions, version: self.version })
    }
    pub fn edit(
        &self,
        range: Range<CharPos>,
        text: &str,
        properties: PropSetId,
    ) -> TextResult<(Self, EditRecord)> {
        let inserted = Snapshot {
            text: Rope::from(text),
            props: PropRuns::uniform(text.chars().count(), properties),
            positions: PositionTree::new(text.chars().count()),
            version: self.version,
        };
        self.splice(range, &inserted)
    }
    /// Replace a range with shared text and properties from another snapshot.
    /// Runtime adapters resolve opaque property IDs in a common owning domain.
    pub fn splice(
        &self,
        range: Range<CharPos>,
        inserted: &Snapshot,
    ) -> TextResult<(Self, EditRecord)> {
        let old_bytes = self.text.char_to_byte(range.start)?..self.text.char_to_byte(range.end)?;
        let old_lines =
            self.text.char_to_line(range.start)?..LinePos(self.text.char_to_line(range.end)?.0 + 1);
        let delta = EditDelta::new(range.clone(), inserted.len_chars())?;
        let (left, tail) = self.text.split_at(range.start)?;
        let (_, right) = tail.split_at(CharPos(range.end.0 - range.start.0))?;
        let next_text = left.checked_append(&inserted.text)?.checked_append(&right)?;
        let next_props = self
            .props
            .slice(CharPos(0)..range.start)?
            .checked_append(&inserted.props)?
            .checked_append(&self.props.slice(range.end..CharPos(self.len_chars()))?)?;
        let next_positions = self.positions.edit(&delta)?;
        let version = Version(self.version.0.checked_add(1).ok_or(TextError::VersionOverflow)?);
        let new_end = CharPos(delta.old.start.0 + delta.inserted_chars);
        let record = EditRecord {
            version,
            old_bytes,
            new_bytes: next_text.char_to_byte(delta.old.start)?..next_text.char_to_byte(new_end)?,
            old_lines,
            new_lines: next_text.char_to_line(delta.old.start)?
                ..LinePos(next_text.char_to_line(new_end)?.0 + 1),
            delta,
        };
        Ok((
            Self { text: next_text, props: next_props, positions: next_positions, version },
            record,
        ))
    }
    pub fn set_properties(&self, range: Range<CharPos>, properties: PropSetId) -> TextResult<Self> {
        Ok(Self {
            text: self.text.clone(),
            props: self.props.set(range, properties)?,
            positions: self.positions.clone(),
            version: Version(self.version.0.checked_add(1).ok_or(TextError::VersionOverflow)?),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_publish_matching_text_properties_and_edit_metrics() {
        let original = Snapshot::new(Rope::from("aé\n🙂z"));
        let (edited, record) =
            original.edit(CharPos(1)..CharPos(3), "中\nλ", PropSetId(7)).unwrap();
        assert_eq!(original.text().to_string(), "aé\n🙂z");
        assert_eq!(edited.text().to_string(), "a中\nλ🙂z");
        assert_eq!(edited.len_chars(), edited.properties().len_chars());
        assert_eq!(record.old_bytes, BytePos(1)..BytePos(4));
        assert_eq!(record.new_bytes, BytePos(1)..BytePos(7));
        assert_eq!(record.delta.map(CharPos(4), Bias::Left).unwrap(), CharPos(5));
        assert_eq!(edited.version(), Version(1));
        assert_eq!(original.version(), Version(0));
        assert!(original.edit(CharPos(4)..CharPos(2), "", PropSetId(0)).is_err());
    }
    #[test]
    fn insertion_bias_and_replacement_suffix_mapping() {
        let insert = EditDelta::new(CharPos(2)..CharPos(2), 3).unwrap();
        assert_eq!(insert.map(CharPos(2), Bias::Left).unwrap(), CharPos(2));
        assert_eq!(insert.map(CharPos(2), Bias::Right).unwrap(), CharPos(5));
        assert_eq!(insert.map(CharPos(4), Bias::Left).unwrap(), CharPos(7));
        let replace = EditDelta::new(CharPos(2)..CharPos(5), 1).unwrap();
        assert_eq!(replace.map(CharPos(5), Bias::Left).unwrap(), CharPos(3));
        assert_eq!(replace.map(CharPos(3), Bias::Right).unwrap(), CharPos(3));
        assert_eq!(replace.map(CharPos(3), Bias::Left).unwrap(), CharPos(2));
    }
    #[test]
    fn snapshot_is_send_sync_and_indexed_line_skip() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Snapshot>();
        let snapshot = Snapshot::new(Rope::from("abc\ndef\n"));
        assert_eq!(snapshot.next_line_start(CharPos(0)), Some(CharPos(4)));
        assert_eq!(snapshot.next_line_start(CharPos(4)), Some(CharPos(8)));
        assert_eq!(snapshot.next_line_start(CharPos(8)), None);
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use crate::MarkerId;
    #[test]
    fn property_preserving_copy_paste_and_marker_publication() {
        let source = Snapshot::new(Rope::from("aé🙂z"))
            .set_properties(CharPos(1)..CharPos(3), PropSetId(9))
            .unwrap()
            .with_marker(MarkerId(1), CharPos(2), Bias::Right)
            .unwrap();
        let payload = source.slice(CharPos(1)..CharPos(4)).unwrap();
        assert_eq!(payload.positions().marker_count(), 0);
        let (next, record) = source.splice(CharPos(0)..CharPos(1), &payload).unwrap();
        assert_eq!(next.text().to_string(), "é🙂zé🙂z");
        assert_eq!(
            next.properties()
                .runs()
                .map(|(r, id)| (r.start.0, r.end.0, id.0))
                .collect::<Vec<_>>(),
            vec![(0, 2, 9), (2, 3, 0), (3, 5, 9), (5, 6, 0)]
        );
        assert_eq!(next.positions().get(MarkerId(1)), Some((CharPos(4), Bias::Right)));
        assert_eq!(source.positions().get(MarkerId(1)), Some((CharPos(2), Bias::Right)));
        assert_eq!(record.version, Version(2));
        assert_eq!(next.without_marker(MarkerId(1)).positions().marker_count(), 0);
        assert_eq!(next.chunks(CharPos(1)..CharPos(4)).unwrap().collect::<String>(), "🙂zé");
        assert!(source.splice(CharPos(3)..CharPos(1), &payload).is_err());
        assert!(source.splice(CharPos(0)..CharPos(9), &payload).is_err());
    }
    #[test]
    fn bounded_display_window_on_long_shared_document() {
        let source = Snapshot::new(Rope::from("é🙂\n".repeat(20000).as_str()));
        let copy = source.clone();
        let payload = source.slice(CharPos(15000)..CharPos(15006)).unwrap();
        let (next, _) = source.splice(CharPos(0)..CharPos(0), &payload).unwrap();
        assert_eq!(copy.len_chars(), 60000);
        assert_eq!(next.len_chars(), 60006);
        assert_eq!(
            next.chunks(CharPos(59997)..CharPos(60006)).unwrap().collect::<String>(),
            "é🙂\né🙂\né🙂\n"
        );
        assert_eq!(next.next_line_start(CharPos(59997)), Some(CharPos(60000)));
    }
}
