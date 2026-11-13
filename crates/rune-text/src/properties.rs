//! Persistent property runs carrying opaque set IDs, never Lisp values.
use crate::{CharPos, TextError, TextResult};
use rune_tree::{Iter, SumTree, Summarize, Summary};
use std::ops::Range;

/// A property-set handle interpreted by the owning document/domain adapter.
/// Zero denotes no properties. IDs from different interners must not be mixed.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub struct PropSetId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    len: usize,
    set: PropSetId,
}
#[derive(Clone, Copy, Default)]
struct RunSummary {
    chars: usize,
}
impl Summary for RunSummary {
    fn combine(&self, other: &Self) -> Self {
        Self { chars: self.chars + other.chars }
    }
}
impl Summarize for Run {
    type Summary = RunSummary;
    fn summarize(&self) -> RunSummary {
        RunSummary { chars: self.len }
    }
}
#[derive(Clone, Default)]
pub struct PropRuns {
    tree: SumTree<Run>,
}
impl std::fmt::Debug for PropRuns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.runs()).finish()
    }
}
impl PropRuns {
    pub fn new(chars: usize) -> Self {
        Self::uniform(chars, PropSetId::default())
    }
    pub fn uniform(chars: usize, set: PropSetId) -> Self {
        Self {
            tree: if chars == 0 {
                SumTree::new()
            } else {
                SumTree::singleton(Run { len: chars, set })
            },
        }
    }
    pub fn len_chars(&self) -> usize {
        self.tree.summary().chars
    }
    pub fn run_count(&self) -> usize {
        self.tree.len()
    }
    pub fn at(&self, position: CharPos) -> Option<PropSetId> {
        self.tree.seek(position.0, |s| s.chars).map(|(_, _, run)| run.set)
    }
    pub fn next_change(&self, position: CharPos) -> Option<CharPos> {
        let (_, before, run) = self.tree.seek(position.0, |s| s.chars)?;
        let end = before.chars + run.len;
        (end < self.len_chars()).then_some(CharPos(end))
    }
    pub fn runs(&self) -> Runs<'_> {
        Runs { iter: self.tree.iter(), position: 0 }
    }
    pub fn runs_in(
        &self,
        range: Range<CharPos>,
    ) -> TextResult<impl Iterator<Item = (Range<CharPos>, PropSetId)> + '_> {
        self.check(&range)?;
        let (index, base) = match self.tree.seek(range.start.0, |s| s.chars) {
            Some((i, b, _)) => (i, b.chars),
            None => (self.tree.len(), self.len_chars()),
        };
        let iter =
            Runs { iter: self.tree.iter_range(index..self.tree.len()).unwrap(), position: base };
        Ok(iter
            .take_while(move |(r, _)| r.start < range.end)
            .map(move |(r, id)| {
                (CharPos(r.start.0.max(range.start.0))..CharPos(r.end.0.min(range.end.0)), id)
            })
            .filter(|(r, _)| r.start < r.end))
    }
    pub fn set(&self, range: Range<CharPos>, set: PropSetId) -> TextResult<Self> {
        self.check(&range)?;
        if range.start == range.end {
            return Ok(self.clone());
        }
        let (a, tail) = self.split(range.start)?;
        let (_, b) = tail.split(CharPos(range.end.0 - range.start.0))?;
        Ok(a.append(&Self::uniform(range.end.0 - range.start.0, set)).append(&b))
    }
    /// Replace deleted text with `inserted` characters of an explicit set.
    /// Stickiness is a policy of the caller, not guessed by the storage layer.
    pub fn replace(
        &self,
        range: Range<CharPos>,
        inserted: usize,
        set: PropSetId,
    ) -> TextResult<Self> {
        self.check(&range)?;
        let (a, tail) = self.split(range.start)?;
        let (_, b) = tail.split(CharPos(range.end.0 - range.start.0))?;
        a.checked_append(&Self::uniform(inserted, set))?.checked_append(&b)
    }
    pub fn slice(&self, range: Range<CharPos>) -> TextResult<Self> {
        self.check(&range)?;
        let (_, tail) = self.split(range.start)?;
        Ok(tail.split(CharPos(range.end.0 - range.start.0))?.0)
    }
    /// Panics if the resulting unit count exceeds usize::MAX.
    pub fn append(&self, other: &Self) -> Self {
        self.checked_append(other).expect("property capacity overflow")
    }
    pub fn checked_append(&self, other: &Self) -> TextResult<Self> {
        self.len_chars()
            .checked_add(other.len_chars())
            .ok_or(TextError::CapacityOverflow)?;
        Ok(self.append_validated(other))
    }
    fn append_validated(&self, other: &Self) -> Self {
        let Some(last) = self.tree.get(self.tree.len().saturating_sub(1)) else {
            return other.clone();
        };
        let Some(first) = other.tree.get(0) else {
            return self.clone();
        };
        if last.set != first.set {
            return Self { tree: self.tree.append(&other.tree) };
        }
        let (a, _) = self.tree.split(self.tree.len() - 1).unwrap();
        let (_, b) = other.tree.split(1).unwrap();
        Self {
            tree: a
                .append(&SumTree::singleton(Run { len: last.len + first.len, set: last.set }))
                .append(&b),
        }
    }
    fn check(&self, r: &Range<CharPos>) -> TextResult<()> {
        if r.start > r.end {
            Err(TextError::ReversedRange)
        } else if r.end.0 > self.len_chars() {
            Err(TextError::OutOfBounds)
        } else {
            Ok(())
        }
    }
    fn split(&self, at: CharPos) -> TextResult<(Self, Self)> {
        if at.0 > self.len_chars() {
            return Err(TextError::OutOfBounds);
        }
        if at.0 == self.len_chars() {
            return Ok((self.clone(), Self::default()));
        }
        let (i, b, run) = self.tree.seek(at.0, |s| s.chars).unwrap();
        let (a, tail) = self.tree.split(i).unwrap();
        if at.0 == b.chars {
            return Ok((Self { tree: a }, Self { tree: tail }));
        }
        let (_, c) = tail.split(1).unwrap();
        let left = at.0 - b.chars;
        Ok((
            Self { tree: a.append(&SumTree::singleton(Run { len: left, set: run.set })) },
            Self { tree: SumTree::singleton(Run { len: run.len - left, set: run.set }).append(&c) },
        ))
    }
}
pub struct Runs<'a> {
    iter: Iter<'a, Run>,
    position: usize,
}
impl Iterator for Runs<'_> {
    type Item = (Range<CharPos>, PropSetId);
    fn next(&mut self) -> Option<Self::Item> {
        let run = self.iter.next()?;
        let start = self.position;
        self.position += run.len;
        Some((CharPos(start)..CharPos(self.position), run.set))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runs_follow_edits_and_snapshots() {
        let original = PropRuns::new(10).set(CharPos(2)..CharPos(7), PropSetId(1)).unwrap();
        let edited = original.replace(CharPos(4)..CharPos(6), 3, PropSetId(2)).unwrap();
        assert_eq!(original.len_chars(), 10);
        assert_eq!(edited.len_chars(), 11);
        assert_eq!(
            edited.runs().map(|(r, id)| (r.start.0, r.end.0, id.0)).collect::<Vec<_>>(),
            vec![(0, 2, 0), (2, 4, 1), (4, 7, 2), (7, 8, 1), (8, 11, 0)]
        );
        let cleared = edited.set(CharPos(2)..CharPos(8), PropSetId(0)).unwrap();
        assert_eq!(cleared.run_count(), 1);
    }
    #[test]
    fn model_based_splices_and_sets() {
        let mut runs = PropRuns::new(20);
        let mut model = vec![0u32; 20];
        let mut seed = 19u64;
        for n in 0..1500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = (seed as usize) % (model.len() + 1);
            let b = a + ((seed >> 32) as usize) % (model.len() - a + 1);
            let id = (seed % 4) as u32;
            if n % 2 == 0 {
                runs = runs.set(CharPos(a)..CharPos(b), PropSetId(id)).unwrap();
                model[a..b].fill(id);
            } else {
                let len = ((seed >> 17) % 6) as usize;
                runs = runs.replace(CharPos(a)..CharPos(b), len, PropSetId(id)).unwrap();
                model.splice(a..b, std::iter::repeat_n(id, len));
            }
            assert_eq!(runs.len_chars(), model.len());
            for (i, id) in model.iter().enumerate() {
                assert_eq!(runs.at(CharPos(i)), Some(PropSetId(*id)));
            }
            let ids = runs.runs().map(|(_, id)| id).collect::<Vec<_>>();
            assert!(ids.windows(2).all(|w| w[0] != w[1]));
        }
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn enormous_uniform_runs_reject_checked_overflow() {
        let huge = PropRuns::uniform(usize::MAX, PropSetId(7));
        for id in [7, 8] {
            let one = PropRuns::uniform(1, PropSetId(id));
            assert!(matches!(huge.checked_append(&one), Err(TextError::CapacityOverflow)));
            assert!(std::panic::catch_unwind(|| huge.append(&one)).is_err());
        }
        assert!(matches!(
            huge.replace(CharPos(0)..CharPos(0), 1, PropSetId(0)),
            Err(TextError::CapacityOverflow)
        ));
        assert_eq!(huge.len_chars(), usize::MAX);
    }
}
