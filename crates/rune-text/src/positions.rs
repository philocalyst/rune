//! Persistent gap-indexed markers. Text edits touch only the affected markers
//! and one suffix gap, not every marker after the edit. No buffer ownership.
use crate::{Bias, CharPos, EditDelta, TextError, TextResult};
use rune_tree::{SumTree, Summarize, Summary};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MarkerId(pub u64);
#[derive(Clone, Copy, Debug)]
struct Marker {
    gap: usize,
    id: MarkerId,
    bias: Bias,
}
#[derive(Clone, Default)]
struct PositionSummary {
    chars: usize,
}
impl Summary for PositionSummary {
    fn combine(&self, right: &Self) -> Self {
        Self { chars: self.chars + right.chars }
    }
}
impl Summarize for Marker {
    type Summary = PositionSummary;
    fn summarize(&self) -> PositionSummary {
        PositionSummary { chars: self.gap }
    }
}
#[derive(Clone, Default)]
pub struct PositionTree {
    tree: SumTree<Marker>,
    text_len: usize,
}
impl std::fmt::Debug for PositionTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.markers()).finish()
    }
}
impl PositionTree {
    pub fn new(text_len: usize) -> Self {
        Self { tree: SumTree::new(), text_len }
    }
    pub fn len_chars(&self) -> usize {
        self.text_len
    }
    pub fn marker_count(&self) -> usize {
        self.tree.len()
    }
    pub fn markers(&self) -> impl Iterator<Item = (MarkerId, CharPos, Bias)> + '_ {
        let mut position = 0;
        self.tree.iter().map(move |m| {
            position += m.gap;
            (m.id, CharPos(position), m.bias)
        })
    }
    /// Inclusive marker window; seeks by cached gaps rather than scanning the prefix.
    pub fn markers_in(
        &self,
        range: std::ops::RangeInclusive<CharPos>,
    ) -> TextResult<impl Iterator<Item = (MarkerId, CharPos, Bias)> + '_> {
        let start = *range.start();
        let end = *range.end();
        if start > end {
            return Err(TextError::ReversedRange);
        }
        if end.0 > self.text_len {
            return Err(TextError::OutOfBounds);
        }
        let (index, before) = self
            .tree
            .seek_end(start.0, |s| s.chars)
            .map(|(i, s, _)| (i, s.chars))
            .unwrap_or((self.tree.len(), self.tree.summary().chars));
        let mut position = before;
        Ok(self
            .tree
            .iter_range(index..self.tree.len())
            .unwrap()
            .map(move |marker| {
                position += marker.gap;
                (marker.id, CharPos(position), marker.bias)
            })
            .take_while(move |(_, p, _)| *p <= end))
    }
    /// ID lookup is linear in this initial index; position edits are indexed.
    pub fn get(&self, id: MarkerId) -> Option<(CharPos, Bias)> {
        self.markers().find(|(key, _, _)| *key == id).map(|(_, p, b)| (p, b))
    }
    pub fn insert(&self, id: MarkerId, position: CharPos, bias: Bias) -> TextResult<Self> {
        if position.0 > self.text_len {
            return Err(TextError::OutOfBounds);
        }
        if self.get(id).is_some() {
            return Err(TextError::DuplicateMarker);
        }
        let (index, before) = self
            .tree
            .seek_end(position.0, |s| s.chars)
            .map(|(i, b, _)| (i, b.chars))
            .unwrap_or((self.tree.len(), self.tree.summary().chars));
        let (left, right) = self.tree.split(index).unwrap();
        let gap = position.0 - before;
        let right = if let Some(first) = right.get(0) {
            with_first_gap(&right, first.gap - gap)
        } else {
            right
        };
        Ok(Self {
            tree: left.append(&SumTree::singleton(Marker { gap, id, bias })).append(&right),
            text_len: self.text_len,
        })
    }
    pub fn remove(&self, id: MarkerId) -> Self {
        let Some(index) = self.tree.iter().position(|m| m.id == id) else {
            return self.clone();
        };
        let (left, tail) = self.tree.split(index).unwrap();
        let removed = tail.get(0).unwrap();
        let (_, right) = tail.split(1).unwrap();
        let right = match right.get(0) {
            Some(first) => with_first_gap(&right, first.gap + removed.gap),
            None => right,
        };
        Self { tree: left.append(&right), text_len: self.text_len }
    }
    pub fn edit(&self, delta: &EditDelta) -> TextResult<Self> {
        if delta.old.start > delta.old.end {
            return Err(TextError::ReversedRange);
        }
        if delta.old.end.0 > self.text_len {
            return Err(TextError::OutOfBounds);
        }
        let new_len = self
            .text_len
            .checked_sub(delta.old.end.0 - delta.old.start.0)
            .and_then(|n| n.checked_add(delta.inserted_chars))
            .ok_or(TextError::OutOfBounds)?;
        let (index, before) = self
            .tree
            .seek_end(delta.old.start.0, |s| s.chars)
            .map(|(i, b, _)| (i, b.chars))
            .unwrap_or((self.tree.len(), self.tree.summary().chars));
        let (left, tail) = self.tree.split(index).unwrap();
        let mut old_position = before;
        let mut affected = 0;
        let mut left_markers = Vec::new();
        let mut right_markers = Vec::new();
        let mut suffix_first = None;
        for marker in tail.iter() {
            old_position += marker.gap;
            if old_position > delta.old.end.0 {
                suffix_first = Some(delta.map(CharPos(old_position), marker.bias)?.0);
                break;
            }
            affected += 1;
            let target = delta.map(CharPos(old_position), marker.bias)?.0;
            if target == delta.old.start.0 {
                left_markers.push((*marker, target));
            } else {
                right_markers.push((*marker, target));
            }
        }
        let (_, suffix) = tail.split(affected).unwrap();
        let mut position = before;
        let mut rebuilt = Vec::new();
        for (mut marker, target) in left_markers.into_iter().chain(right_markers) {
            marker.gap = target - position;
            position = target;
            rebuilt.push(marker);
        }
        let suffix = if let Some(target) = suffix_first {
            with_first_gap(&suffix, target - position)
        } else {
            suffix
        };
        Ok(Self {
            tree: left.append(&rebuilt.into_iter().collect()).append(&suffix),
            text_len: new_len,
        })
    }
}
fn with_first_gap(tree: &SumTree<Marker>, gap: usize) -> SumTree<Marker> {
    let mut first = *tree.get(0).unwrap();
    first.gap = gap;
    let (_, tail) = tree.split(1).unwrap();
    SumTree::singleton(first).append(&tail)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_position_bias_and_deleted_markers() {
        let tree = PositionTree::new(10)
            .insert(MarkerId(1), CharPos(3), Bias::Left)
            .unwrap()
            .insert(MarkerId(2), CharPos(3), Bias::Right)
            .unwrap()
            .insert(MarkerId(3), CharPos(6), Bias::Left)
            .unwrap();
        let shifted = tree.edit(&EditDelta::new(CharPos(3)..CharPos(3), 2).unwrap()).unwrap();
        assert_eq!(shifted.get(MarkerId(1)).unwrap().0, CharPos(3));
        assert_eq!(shifted.get(MarkerId(2)).unwrap().0, CharPos(5));
        assert_eq!(shifted.get(MarkerId(3)).unwrap().0, CharPos(8));
        assert_eq!(tree.get(MarkerId(3)).unwrap().0, CharPos(6));
        let deleted = shifted.edit(&EditDelta::new(CharPos(2)..CharPos(9), 0).unwrap()).unwrap();
        assert!(deleted.markers().all(|(_, p, _)| p == CharPos(2)));
        assert_eq!(deleted.remove(MarkerId(2)).marker_count(), 2);
    }
    #[test]
    fn marker_edits_match_naive_model() {
        let mut tree = PositionTree::new(100);
        let mut model = Vec::new();
        for id in 0..300 {
            let p = CharPos((id * 17 % 101) as usize);
            let bias = if id % 2 == 0 { Bias::Left } else { Bias::Right };
            tree = tree.insert(MarkerId(id), p, bias).unwrap();
            model.push((MarkerId(id), p, bias));
        }
        let mut seed = 11u64;
        for _ in 0..800 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = (seed as usize) % (tree.len_chars() + 1);
            let b = a + ((seed >> 32) as usize) % (tree.len_chars() - a + 1);
            let delta =
                EditDelta::new(CharPos(a)..CharPos(b), ((seed >> 19) % 8) as usize).unwrap();
            tree = tree.edit(&delta).unwrap();
            for (id, p, bias) in &mut model {
                *p = delta.map(*p, *bias).unwrap();
                assert_eq!(tree.get(*id), Some((*p, *bias)));
            }
            let positions = tree.markers().map(|(_, p, _)| p).collect::<Vec<_>>();
            assert!(positions.windows(2).all(|w| w[0] <= w[1]));
        }
    }
}
