//! Indexed adapter over the text lane's immutable persistent snapshot.
use crate::TextSnapshot;
use rune_text::{CharPos, Snapshot};

#[derive(Clone, Debug)]
pub struct RopeSnapshot(pub Snapshot);

impl TextSnapshot for RopeSnapshot {
    fn revision(&self) -> u64 {
        self.0.version().0
    }
    fn len_chars(&self) -> usize {
        self.0.len_chars()
    }
    fn chars_from(&self, position: usize) -> Box<dyn Iterator<Item = char> + '_> {
        // Match StringSnapshot's clamped EOF behavior without copying a prefix.
        Box::new(
            self.0
                .chars_from(CharPos(position.min(self.0.len_chars())))
                .expect("position was clamped to snapshot bounds"),
        )
    }
    fn next_line_start(&self, position: usize) -> Option<usize> {
        self.0.next_line_start(CharPos(position)).map(|position| position.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune_text::{PropSetId, Rope};
    #[test]
    fn unicode_seek_revision_and_immutable_edit() {
        let original = RopeSnapshot(Snapshot::new(Rope::from("aé\n🙂z")));
        let (edited, _) = original.0.edit(CharPos(1)..CharPos(2), "中", PropSetId(0)).unwrap();
        assert_eq!(original.chars_from(1).collect::<String>(), "é\n🙂z");
        assert_eq!(original.next_line_start(0), Some(3));
        assert_eq!(original.next_line_start(3), None);
        assert_eq!(original.chars_from(usize::MAX).count(), 0);
        let edited = RopeSnapshot(edited);
        assert_eq!(edited.revision(), 1);
        assert_eq!(edited.chars_from(1).collect::<String>(), "中\n🙂z");
    }
}
