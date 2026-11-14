//! Sparse deletion-marker corrections. This supplements, not replaces, live marker mapping.
//! Records have no document or GC domain identity; the runtime validates those.
use crate::{Bias, CharPos, EditDelta, MarkerId, PositionTree, TextError, TextResult};
use std::ops::Range;
#[derive(Clone, Copy, Debug)]
enum Adjustment {
    Add(usize),
    Subtract(usize),
}
#[derive(Clone, Copy, Debug)]
struct Record {
    id: MarkerId,
    adjustment: Adjustment,
}
#[derive(Clone, Debug)]
pub struct DeletionMarkers {
    boundary: CharPos,
    deleted_len: usize,
    records: Vec<Record>,
}
#[derive(Clone, Debug)]
pub struct PreparedMarkerUndo {
    records: Vec<Record>,
}
impl DeletionMarkers {
    /// Inclusive endpoints are intentional: left end and right start markers
    /// can need correction. Zero adjustments are omitted, not the deletion.
    pub fn capture(positions: &PositionTree, range: Range<CharPos>) -> TextResult<Self> {
        if range.start > range.end {
            return Err(TextError::ReversedRange);
        }
        if range.end.0 > positions.len_chars() {
            return Err(TextError::OutOfBounds);
        }
        let mut records = Vec::new();
        if range.start < range.end {
            for (id, position, bias) in positions.markers_in(range.start..=range.end)? {
                let adjustment = match bias {
                    Bias::Left if position > range.start => {
                        Some(Adjustment::Add(position.0 - range.start.0))
                    }
                    Bias::Right if position < range.end => {
                        Some(Adjustment::Subtract(range.end.0 - position.0))
                    }
                    _ => None,
                };
                if let Some(adjustment) = adjustment {
                    records.push(Record { id, adjustment })
                }
            }
        }
        Ok(Self { boundary: range.start, deleted_len: range.end.0 - range.start.0, records })
    }
    pub fn boundary(&self) -> CharPos {
        self.boundary
    }
    pub fn deleted_len(&self) -> usize {
        self.deleted_len
    }
    pub fn record_count(&self) -> usize {
        self.records.len()
    }
    pub(crate) fn matches_undo(&self, delta: &EditDelta) -> bool {
        delta.old.start == self.boundary && delta.inserted_chars == self.deleted_len
    }
    /// Validate BEFORE reinsertion. Moved-away or absent IDs are ignored;
    /// moving back to the boundary makes a recorded marker eligible again.
    pub fn prepare(&self, current: &PositionTree) -> PreparedMarkerUndo {
        PreparedMarkerUndo {
            records: self
                .records
                .iter()
                .filter(|r| current.get(r.id).is_some_and(|(p, _)| p == self.boundary))
                .copied()
                .collect(),
        }
    }
}
impl PreparedMarkerUndo {
    /// Apply AFTER insertion/hooks, only to still-present IDs. Clamp to text
    /// bounds like marker relocation; never recreate a removed registration.
    pub fn apply(&self, after: &PositionTree) -> TextResult<PositionTree> {
        let mut result = after.clone();
        for record in &self.records {
            let Some((position, bias)) = result.get(record.id) else { continue };
            let target = match record.adjustment {
                Adjustment::Add(n) => position.0.saturating_add(n).min(result.len_chars()),
                Adjustment::Subtract(n) => position.0.saturating_sub(n),
            };
            result = result.remove(record.id).insert(record.id, CharPos(target), bias)?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_biases_and_inclusive_deletion_boundaries_recover() {
        let mut original = PositionTree::new(4);
        for (id, p, bias) in [
            (1, 0, Bias::Left),
            (2, 0, Bias::Right),
            (3, 2, Bias::Left),
            (4, 2, Bias::Right),
            (5, 3, Bias::Left),
            (6, 3, Bias::Right),
        ] {
            original = original.insert(MarkerId(id), CharPos(p), bias).unwrap();
        }
        let records = DeletionMarkers::capture(&original, CharPos(0)..CharPos(3)).unwrap();
        assert_eq!(records.record_count(), 4);
        let deleted = original.edit(&EditDelta::new(CharPos(0)..CharPos(3), 0).unwrap()).unwrap();
        let prepared = records.prepare(&deleted);
        let inserted = deleted.edit(&EditDelta::new(CharPos(0)..CharPos(0), 3).unwrap()).unwrap();
        let restored = prepared.apply(&inserted).unwrap();
        for id in 1..=6 {
            assert_eq!(restored.get(MarkerId(id)), original.get(MarkerId(id)));
        }
    }
    #[test]
    fn removed_new_and_moved_ids_are_not_historically_reset() {
        let original = PositionTree::new(4)
            .insert(MarkerId(1), CharPos(2), Bias::Left)
            .unwrap()
            .insert(MarkerId(2), CharPos(2), Bias::Left)
            .unwrap();
        let records = DeletionMarkers::capture(&original, CharPos(0)..CharPos(3)).unwrap();
        let deleted = original.edit(&EditDelta::new(CharPos(0)..CharPos(3), 0).unwrap()).unwrap();
        let current = deleted
            .remove(MarkerId(1))
            .remove(MarkerId(2))
            .insert(MarkerId(2), CharPos(1), Bias::Left)
            .unwrap()
            .insert(MarkerId(3), CharPos(0), Bias::Left)
            .unwrap();
        let prepared = records.prepare(&current);
        let inserted = current.edit(&EditDelta::new(CharPos(0)..CharPos(0), 3).unwrap()).unwrap();
        let result = prepared.apply(&inserted).unwrap();
        assert_eq!(result.get(MarkerId(1)), None);
        assert_eq!(result.get(MarkerId(2)).unwrap().0, CharPos(4));
        assert_eq!(result.get(MarkerId(3)).unwrap().0, CharPos(0));
        let back = current.remove(MarkerId(2)).insert(MarkerId(2), CharPos(0), Bias::Left).unwrap();
        let prepared = records.prepare(&back);
        let inserted = back.edit(&EditDelta::new(CharPos(0)..CharPos(0), 3).unwrap()).unwrap();
        assert_eq!(prepared.apply(&inserted).unwrap().get(MarkerId(2)).unwrap().0, CharPos(2));
        assert_eq!(prepared.apply(&inserted.remove(MarkerId(2))).unwrap().get(MarkerId(2)), None);
    }
}
