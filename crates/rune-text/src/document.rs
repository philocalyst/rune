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
        self.splice_impl(range, inserted, false)
    }
    /// Persistent insertion that advances all existing markers at the boundary.
    /// Inserted snapshots contribute text/properties, never marker ownership.
    /// Runtime callers must validate the common property-ID owner domain.
    pub fn insert_before_markers(&self, at: CharPos, inserted: &Snapshot) -> TextResult<(Self, EditRecord)> {
        self.splice_impl(at..at, inserted, true)
    }
    fn splice_impl(&self, range: Range<CharPos>, inserted: &Snapshot, advance_boundary: bool) -> TextResult<(Self, EditRecord)> {
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
        let next_positions = if advance_boundary {
            self.positions.insert_before_markers(range.start, inserted.len_chars())?
        } else { self.positions.edit(&delta)? };
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
    /// Restore content, never historical marker registrations. The caller
    /// validates history lineage and the common property-ID owner domain.
    /// None means metadata-only publication and preserves current positions.
    pub fn restore_content(&self, content: &Self, delta: Option<&EditDelta>) -> TextResult<Self> {
        let version = Version(self.version.0.checked_add(1).ok_or(TextError::VersionOverflow)?);
        let positions = match delta {
            Some(delta) => self.positions.edit(delta)?,
            None => self.positions.clone(),
        };
        if positions.len_chars() != content.len_chars() {
            return Err(TextError::OutOfBounds);
        }
        Ok(Self { text: content.text.clone(), props: content.props.clone(), positions, version })
    }
    /// Undo OR redo a deletion/replacement with records for reinserted text.
    /// Remove the current inserted span first, validate boundary eligibility,
    /// reinsert old text, then correct surviving IDs. Hooks are not run here.
    pub fn restore_content_with_marker_undo(
        &self,
        content: &Self,
        inverse: &EditDelta,
        records: &crate::DeletionMarkers,
    ) -> TextResult<Self> {
        if !records.matches_undo(inverse) {
            return Err(TextError::OutOfBounds);
        }
        let version = Version(self.version.0.checked_add(1).ok_or(TextError::VersionOverflow)?);
        let intermediate = self.positions.edit(&EditDelta::new(inverse.old.clone(), 0)?)?;
        let prepared = records.prepare(&intermediate);
        let inserted = intermediate
            .edit(&EditDelta::new(inverse.old.start..inverse.old.start, inverse.inserted_chars)?)?;
        let positions = prepared.apply(&inserted)?;
        if positions.len_chars() != content.len_chars() {
            return Err(TextError::OutOfBounds);
        }
        Ok(Self { text: content.text.clone(), props: content.props.clone(), positions, version })
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

#[cfg(test)]
mod recovered_history_tests {
    use super::*;
    use crate::{DeletionMarkers, MarkerId};
    #[test]
    fn history_and_metadata_preserve_current_marker_lifecycle() {
        let before = Snapshot::new(Rope::from("abcd"))
            .with_marker(MarkerId(1), CharPos(2), Bias::Left)
            .unwrap();
        let (after, _) = before.edit(CharPos(0)..CharPos(0), "λ", PropSetId(0)).unwrap();
        let current = after
            .without_marker(MarkerId(1))
            .with_marker(MarkerId(2), CharPos(4), Bias::Left)
            .unwrap();
        let undone = current
            .restore_content(&before, Some(&EditDelta::new(CharPos(0)..CharPos(1), 0).unwrap()))
            .unwrap();
        assert_eq!(undone.positions().get(MarkerId(1)), None);
        assert_eq!(undone.positions().get(MarkerId(2)).unwrap().0, CharPos(3));
        assert_eq!(undone.version(), Version(2));
        let prepared = before.set_properties(CharPos(0)..CharPos(1), PropSetId(9)).unwrap();
        let published = undone.restore_content(&prepared, None).unwrap();
        assert_eq!(published.positions().get(MarkerId(1)), None);
        assert_eq!(published.positions().get(MarkerId(2)).unwrap().0, CharPos(3));
        assert_eq!(published.properties().at(CharPos(0)), Some(PropSetId(9)));
        assert_eq!(published.version(), Version(3));
        assert!(current.restore_content(&before, None).is_err());
        assert_eq!(current.len_chars(), 5);
    }
    #[test]
    fn replacement_and_dynamic_redo_restore_sparse_adjustments() {
        let before = Snapshot::new(Rope::from("abcde"))
            .with_marker(MarkerId(1), CharPos(2), Bias::Left)
            .unwrap()
            .with_marker(MarkerId(2), CharPos(3), Bias::Right)
            .unwrap()
            .with_marker(MarkerId(3), CharPos(4), Bias::Left)
            .unwrap();
        let records = DeletionMarkers::capture(before.positions(), CharPos(1)..CharPos(4)).unwrap();
        let (after, _) = before.edit(CharPos(1)..CharPos(4), "XY", PropSetId(0)).unwrap();
        let current = after
            .without_marker(MarkerId(1))
            .with_marker(MarkerId(4), CharPos(2), Bias::Left)
            .unwrap();
        let undone = current
            .restore_content_with_marker_undo(
                &before,
                &EditDelta::new(CharPos(1)..CharPos(3), 3).unwrap(),
                &records,
            )
            .unwrap();
        assert_eq!(undone.positions().get(MarkerId(1)), None);
        assert_eq!(undone.positions().get(MarkerId(2)).unwrap().0, CharPos(3));
        assert_eq!(undone.positions().get(MarkerId(3)).unwrap().0, CharPos(4));
        assert_eq!(undone.positions().get(MarkerId(4)).unwrap().0, CharPos(1));
        let empty = Snapshot::new(Rope::new());
        let (inserted, _) = empty.edit(CharPos(0)..CharPos(0), "λ🙂", PropSetId(0)).unwrap();
        let current = inserted.with_marker(MarkerId(9), CharPos(2), Bias::Left).unwrap();
        let redo_records =
            DeletionMarkers::capture(current.positions(), CharPos(0)..CharPos(2)).unwrap();
        let undone = current
            .restore_content(&empty, Some(&EditDelta::new(CharPos(0)..CharPos(2), 0).unwrap()))
            .unwrap();
        let redone = undone
            .restore_content_with_marker_undo(
                &inserted,
                &EditDelta::new(CharPos(0)..CharPos(0), 2).unwrap(),
                &redo_records,
            )
            .unwrap();
        assert_eq!(redone.positions().get(MarkerId(9)).unwrap().0, CharPos(2));
    }
    #[test]
    fn empty_sparse_records_still_use_delete_then_insert_mapping() {
        let before = Snapshot::new(Rope::from("abcd"))
            .with_marker(MarkerId(1), CharPos(0), Bias::Left)
            .unwrap()
            .with_marker(MarkerId(2), CharPos(3), Bias::Right)
            .unwrap();
        let records = DeletionMarkers::capture(before.positions(), CharPos(0)..CharPos(3)).unwrap();
        assert_eq!(records.record_count(), 0);
        let (after, _) = before.edit(CharPos(0)..CharPos(3), "X", PropSetId(0)).unwrap();
        let current = after.with_marker(MarkerId(3), CharPos(1), Bias::Left).unwrap();
        let restored = current
            .restore_content_with_marker_undo(
                &before,
                &EditDelta::new(CharPos(0)..CharPos(1), 3).unwrap(),
                &records,
            )
            .unwrap();
        assert_eq!(restored.positions().get(MarkerId(3)).unwrap().0, CharPos(0));
    }
}

#[cfg(test)]
mod insert_before_marker_snapshot_tests {
    use super::*;
    use crate::{DeletionMarkers, MarkerId};
    #[test]
    fn unicode_properties_and_dynamic_marker_undo_preserve_live_ids() {
        let old = Snapshot::new(Rope::from("ab"))
            .set_properties(CharPos(0)..CharPos(2), PropSetId(3)).unwrap()
            .with_marker(MarkerId(1), CharPos(1), Bias::Left).unwrap()
            .with_marker(MarkerId(2), CharPos(1), Bias::Right).unwrap();
        let inserted = Snapshot::new(Rope::from("λ🙂"))
            .set_properties(CharPos(0)..CharPos(2), PropSetId(7)).unwrap()
            .with_marker(MarkerId(99), CharPos(0), Bias::Left).unwrap();
        let (next, record) = old.insert_before_markers(CharPos(1), &inserted).unwrap();
        assert_eq!(next.text().to_string(), "aλ🙂b");
        assert_eq!(next.positions().get(MarkerId(1)), Some((CharPos(3), Bias::Left)));
        assert_eq!(next.positions().get(MarkerId(2)), Some((CharPos(3), Bias::Right)));
        assert_eq!(next.positions().get(MarkerId(99)), None);
        assert_eq!(next.properties().at(CharPos(0)), Some(PropSetId(3)));
        assert_eq!(next.properties().at(CharPos(1)), Some(PropSetId(7)));
        assert_eq!(next.properties().at(CharPos(2)), Some(PropSetId(7)));
        assert_eq!(next.properties().at(CharPos(3)), Some(PropSetId(3)));
        assert_eq!(record.delta.inserted_chars, 2);
        assert_eq!(old.positions().get(MarkerId(1)).unwrap().0, CharPos(1));
        let markers = DeletionMarkers::capture(next.positions(), CharPos(1)..CharPos(3)).unwrap();
        let inverse = EditDelta::new(CharPos(1)..CharPos(3), 0).unwrap();
        let undone = next.restore_content(&old, Some(&inverse)).unwrap()
            .with_marker(MarkerId(5), CharPos(1), Bias::Left).unwrap();
        let redo = undone.restore_content_with_marker_undo(&next, &record.delta, &markers).unwrap();
        assert_eq!(redo.positions().get(MarkerId(1)).unwrap().0, CharPos(3));
        assert_eq!(redo.positions().get(MarkerId(2)).unwrap().0, CharPos(3));
        assert_eq!(redo.positions().get(MarkerId(5)).unwrap().0, CharPos(1));
        let removed = undone.without_marker(MarkerId(1))
            .restore_content_with_marker_undo(&next, &record.delta, &markers).unwrap();
        assert_eq!(removed.positions().get(MarkerId(1)), None);
    }
}
