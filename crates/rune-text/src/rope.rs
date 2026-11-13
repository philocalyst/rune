use rune_tree::{Iter, SumTree, Summarize, Summary};
use std::{fmt, ops::Range, sync::Arc};

macro_rules! coordinate {
    ($name:ident) => {
        #[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub usize);
    };
}
coordinate!(CharPos);
coordinate!(BytePos);
coordinate!(LinePos);
coordinate!(Utf16Pos);
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextError {
    OutOfBounds,
    ReversedRange,
    NotCharBoundary,
    VersionOverflow,
    DuplicateMarker,
    CapacityOverflow,
}
impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::OutOfBounds => "text position out of bounds",
                Self::ReversedRange => "text range is reversed",
                Self::NotCharBoundary => "position splits a character",
                Self::VersionOverflow => "text version overflow",
                Self::DuplicateMarker => "duplicate marker ID",
                Self::CapacityOverflow => "text capacity overflow",
            }
        )
    }
}
impl std::error::Error for TextError {}
pub type TextResult<T> = Result<T, TextError>;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TextSummary {
    pub bytes: usize,
    pub chars: usize,
    pub newlines: usize,
    pub utf16: usize,
}
impl Summary for TextSummary {
    fn combine(&self, other: &Self) -> Self {
        Self {
            bytes: self.bytes + other.bytes,
            chars: self.chars + other.chars,
            newlines: self.newlines + other.newlines,
            utf16: self.utf16 + other.utf16,
        }
    }
}
impl TextSummary {
    fn of(text: &str) -> Self {
        Self {
            bytes: text.len(),
            chars: text.chars().count(),
            newlines: text.bytes().filter(|b| *b == b'\n').count(),
            utf16: text.chars().map(char::len_utf16).sum(),
        }
    }
}
const CHUNK_BYTES: usize = 2048;
#[derive(Debug)]
struct Chunk {
    source: Arc<str>,
    range: Range<usize>,
    summary: TextSummary,
}
impl Chunk {
    fn text(&self) -> &str {
        &self.source[self.range.clone()]
    }
    fn new(source: Arc<str>, range: Range<usize>) -> Self {
        let summary = TextSummary::of(&source[range.clone()]);
        Self { source, range, summary }
    }
    fn part(&self, range: Range<usize>) -> Self {
        Self::new(
            self.source.clone(),
            self.range.start + range.start..self.range.start + range.end,
        )
    }
}
impl Summarize for Chunk {
    type Summary = TextSummary;
    fn summarize(&self) -> TextSummary {
        self.summary
    }
}

/// UTF-8 rope. Clone is O(1), seeks and edits are O(log(chunks)) plus bounded
/// leaf scanning; construction is linear. Unchanged leaves share Arc storage.
#[derive(Clone, Default)]
pub struct Rope {
    tree: SumTree<Chunk>,
}
impl fmt::Debug for Rope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rope")
            .field("summary", &self.summary())
            .field("chunks", &self.tree.len())
            .finish()
    }
}
impl From<&str> for Rope {
    fn from(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        let source: Arc<str> = Arc::from(text);
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < source.len() {
            let mut end = (start + CHUNK_BYTES).min(source.len());
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            chunks.push(Chunk::new(source.clone(), start..end));
            start = end;
        }
        Self { tree: chunks.into_iter().collect() }
    }
}
impl fmt::Display for Rope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.tree.iter() {
            f.write_str(chunk.text())?;
        }
        Ok(())
    }
}
impl Rope {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn summary(&self) -> TextSummary {
        self.tree.summary()
    }
    pub fn len_chars(&self) -> usize {
        self.summary().chars
    }
    pub fn len_bytes(&self) -> usize {
        self.summary().bytes
    }
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }
    pub fn line_count(&self) -> usize {
        self.summary().newlines + 1
    }
    pub fn char_to_byte(&self, at: CharPos) -> TextResult<BytePos> {
        if at.0 == self.len_chars() {
            return Ok(BytePos(self.len_bytes()));
        }
        let (_, before, chunk) = self.tree.seek(at.0, |s| s.chars).ok_or(TextError::OutOfBounds)?;
        Ok(BytePos(before.bytes + byte_at_char(chunk.text(), at.0 - before.chars)))
    }
    pub fn byte_to_char(&self, at: BytePos) -> TextResult<CharPos> {
        if at.0 == self.len_bytes() {
            return Ok(CharPos(self.len_chars()));
        }
        let (_, before, chunk) = self.tree.seek(at.0, |s| s.bytes).ok_or(TextError::OutOfBounds)?;
        let local = at.0 - before.bytes;
        if !chunk.text().is_char_boundary(local) {
            return Err(TextError::NotCharBoundary);
        }
        Ok(CharPos(before.chars + chunk.text()[..local].chars().count()))
    }
    pub fn char_to_line(&self, at: CharPos) -> TextResult<LinePos> {
        if at.0 == self.len_chars() {
            return Ok(LinePos(self.summary().newlines));
        }
        let (_, before, chunk) = self.tree.seek(at.0, |s| s.chars).ok_or(TextError::OutOfBounds)?;
        let end = byte_at_char(chunk.text(), at.0 - before.chars);
        Ok(LinePos(
            before.newlines + chunk.text()[..end].bytes().filter(|b| *b == b'\n').count(),
        ))
    }
    pub fn line_to_char(&self, at: LinePos) -> TextResult<CharPos> {
        if at.0 == 0 {
            return Ok(CharPos(0));
        }
        let (_, before, chunk) =
            self.tree.seek(at.0 - 1, |s| s.newlines).ok_or(TextError::OutOfBounds)?;
        let newline = at.0 - 1 - before.newlines;
        let local = chunk
            .text()
            .chars()
            .enumerate()
            .filter(|(_, c)| *c == '\n')
            .nth(newline)
            .unwrap()
            .0;
        Ok(CharPos(before.chars + local + 1))
    }
    /// First line start strictly after `at`, using cached newline summaries.
    pub fn next_line_start(&self, at: CharPos) -> Option<CharPos> {
        let line = self.char_to_line(at).ok()?;
        self.line_to_char(LinePos(line.0.checked_add(1)?)).ok()
    }
    pub fn char_to_utf16(&self, at: CharPos) -> TextResult<Utf16Pos> {
        if at.0 == self.len_chars() {
            return Ok(Utf16Pos(self.summary().utf16));
        }
        let (_, before, chunk) = self.tree.seek(at.0, |s| s.chars).ok_or(TextError::OutOfBounds)?;
        Ok(Utf16Pos(
            before.utf16
                + chunk
                    .text()
                    .chars()
                    .take(at.0 - before.chars)
                    .map(char::len_utf16)
                    .sum::<usize>(),
        ))
    }
    pub fn utf16_to_char(&self, at: Utf16Pos) -> TextResult<CharPos> {
        if at.0 == self.summary().utf16 {
            return Ok(CharPos(self.len_chars()));
        }
        let (_, before, chunk) = self.tree.seek(at.0, |s| s.utf16).ok_or(TextError::OutOfBounds)?;
        let mut units = before.utf16;
        for (i, ch) in chunk.text().chars().enumerate() {
            if units == at.0 {
                return Ok(CharPos(before.chars + i));
            }
            units += ch.len_utf16();
            if units > at.0 {
                return Err(TextError::NotCharBoundary);
            }
        }
        Err(TextError::OutOfBounds)
    }
    pub fn char_at(&self, at: CharPos) -> Option<char> {
        let (_, before, chunk) = self.tree.seek(at.0, |s| s.chars)?;
        chunk.text().chars().nth(at.0 - before.chars)
    }
    pub fn split_at(&self, at: CharPos) -> TextResult<(Self, Self)> {
        if at.0 > self.len_chars() {
            return Err(TextError::OutOfBounds);
        }
        if at.0 == self.len_chars() {
            return Ok((self.clone(), Self::new()));
        }
        let (index, before, chunk) = self.tree.seek(at.0, |s| s.chars).unwrap();
        let (left, rest) = self.tree.split(index).unwrap();
        if at.0 == before.chars {
            return Ok((Self { tree: left }, Self { tree: rest }));
        }
        let (_, right) = rest.split(1).unwrap();
        let byte = byte_at_char(chunk.text(), at.0 - before.chars);
        Ok((
            Self { tree: left.append(&SumTree::singleton(chunk.part(0..byte))) },
            Self { tree: SumTree::singleton(chunk.part(byte..chunk.text().len())).append(&right) },
        ))
    }
    /// Panics on metadata capacity overflow; use `checked_append` for input
    /// whose size is not already bounded by the caller.
    pub fn append(&self, other: &Self) -> Self {
        self.checked_append(other).expect("rope capacity overflow")
    }
    pub fn checked_append(&self, other: &Self) -> TextResult<Self> {
        // Reserve one position for the line-count endpoint. Each nonempty
        // leaf, scalar, UTF-16 unit and LF contributes at most its byte count.
        self.len_bytes()
            .checked_add(other.len_bytes())
            .filter(|n| *n < usize::MAX)
            .ok_or(TextError::CapacityOverflow)?;
        Ok(Self { tree: self.tree.append(&other.tree) })
    }
    pub fn slice(&self, range: Range<CharPos>) -> TextResult<Self> {
        self.check_range(&range)?;
        let (_, tail) = self.split_at(range.start)?;
        Ok(tail.split_at(CharPos(range.end.0 - range.start.0))?.0)
    }
    pub fn replace(&self, range: Range<CharPos>, text: &str) -> TextResult<Self> {
        self.check_range(&range)?;
        let (left, tail) = self.split_at(range.start)?;
        let (_, right) = tail.split_at(CharPos(range.end.0 - range.start.0))?;
        left.checked_append(&Self::from(text))?.checked_append(&right)
    }
    fn check_range(&self, range: &Range<CharPos>) -> TextResult<()> {
        if range.start > range.end {
            Err(TextError::ReversedRange)
        } else if range.end.0 > self.len_chars() {
            Err(TextError::OutOfBounds)
        } else {
            Ok(())
        }
    }
    /// Borrow only the requested byte window. Iterator setup seeks in O(log n),
    /// iteration is linear in visited chunks, and both ends are supported.
    pub fn chunks(&self, range: Range<BytePos>) -> TextResult<Chunks<'_>> {
        if range.start > range.end {
            return Err(TextError::ReversedRange);
        }
        self.byte_to_char(range.start)?;
        self.byte_to_char(range.end)?;
        if range.start == range.end {
            return Ok(Chunks {
                iter: self.tree.iter_range(0..0).unwrap(),
                front: 0,
                back: 0,
                start: 0,
                end: 0,
            });
        }
        let (first, before, _) = self.tree.seek(range.start.0, |s| s.bytes).unwrap();
        let (last, tail, chunk) = self.tree.seek(range.end.0 - 1, |s| s.bytes).unwrap();
        Ok(Chunks {
            iter: self.tree.iter_range(first..last + 1).unwrap(),
            front: before.bytes,
            back: tail.bytes + chunk.summary.bytes,
            start: range.start.0,
            end: range.end.0,
        })
    }
    pub fn chars_from(&self, at: CharPos) -> TextResult<Chars<'_>> {
        if at.0 > self.len_chars() {
            return Err(TextError::OutOfBounds);
        }
        let (index, skip) = match self.tree.seek(at.0, |s| s.chars) {
            Some((i, b, c)) => (i, byte_at_char(c.text(), at.0 - b.chars)),
            None => (self.tree.len(), 0),
        };
        Ok(Chars {
            chunks: self.tree.iter_range(index..self.tree.len()).unwrap(),
            current: "".chars(),
            skip,
        })
    }
    pub fn cursor(&self, at: BytePos) -> TextResult<Cursor<'_>> {
        let position = self.byte_to_char(at)?;
        Ok(Cursor { rope: self, position })
    }
}
fn byte_at_char(text: &str, at: usize) -> usize {
    text.char_indices().nth(at).map_or(text.len(), |(i, _)| i)
}
pub struct Chunks<'a> {
    iter: Iter<'a, Chunk>,
    front: usize,
    back: usize,
    start: usize,
    end: usize,
}
impl<'a> Iterator for Chunks<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<Self::Item> {
        let chunk = self.iter.next()?;
        let base = self.front;
        self.front += chunk.summary.bytes;
        Some(
            &chunk.text()
                [self.start.saturating_sub(base)..(self.end - base).min(chunk.summary.bytes)],
        )
    }
}
impl DoubleEndedIterator for Chunks<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let chunk = self.iter.next_back()?;
        self.back -= chunk.summary.bytes;
        let base = self.back;
        Some(
            &chunk.text()
                [self.start.saturating_sub(base)..(self.end - base).min(chunk.summary.bytes)],
        )
    }
}
pub struct Chars<'a> {
    chunks: Iter<'a, Chunk>,
    current: std::str::Chars<'a>,
    skip: usize,
}
impl Iterator for Chars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        loop {
            if let Some(ch) = self.current.next() {
                return Some(ch);
            }
            let chunk = self.chunks.next()?;
            self.current = chunk.text()[self.skip..].chars();
            self.skip = 0;
        }
    }
}
/// A seekable borrowed cursor. Sequential render scans should use `chars_from`
/// to amortize tree traversal; cursor seeks and reverse steps are O(log n).
pub struct Cursor<'a> {
    rope: &'a Rope,
    position: CharPos,
}
impl Cursor<'_> {
    pub fn position(&self) -> CharPos {
        self.position
    }
    pub fn byte_position(&self) -> BytePos {
        self.rope.char_to_byte(self.position).unwrap()
    }
    pub fn next_char(&mut self) -> Option<char> {
        let ch = self.rope.char_at(self.position)?;
        self.position.0 += 1;
        Some(ch)
    }
    pub fn previous_char(&mut self) -> Option<char> {
        if self.position.0 == 0 {
            return None;
        }
        self.position.0 -= 1;
        self.rope.char_at(self.position)
    }
    pub fn seek(&mut self, at: BytePos) -> TextResult<()> {
        self.position = self.rope.byte_to_char(at)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_coordinates_round_trip_unicode_and_lines() {
        let text = "aé🙂\n中\n";
        let rope = Rope::from(text);
        assert_eq!(rope.summary(), TextSummary { bytes: 12, chars: 6, newlines: 2, utf16: 7 });
        for (i, (byte, _)) in
            text.char_indices().chain(std::iter::once((text.len(), '\0'))).enumerate()
        {
            assert_eq!(rope.char_to_byte(CharPos(i)).unwrap(), BytePos(byte));
            assert_eq!(rope.byte_to_char(BytePos(byte)).unwrap(), CharPos(i));
            let u = rope.char_to_utf16(CharPos(i)).unwrap();
            assert_eq!(rope.utf16_to_char(u).unwrap(), CharPos(i));
        }
        assert_eq!(rope.utf16_to_char(Utf16Pos(3)), Err(TextError::NotCharBoundary));
        assert_eq!(rope.line_to_char(LinePos(1)).unwrap(), CharPos(4));
        assert_eq!(rope.line_to_char(LinePos(2)).unwrap(), CharPos(6));
        assert!(rope.line_to_char(LinePos(3)).is_err());
    }
    #[test]
    fn persistent_edits_and_bounded_chunk_reads() {
        let text = "aé🙂\n".repeat(2000);
        let original = Rope::from(text.as_str());
        let edited = original.replace(CharPos(1)..CharPos(5000), "replacement🙂").unwrap();
        assert_eq!(original.to_string(), text);
        let model = format!("a{}{}", "replacement🙂", text.chars().skip(5000).collect::<String>());
        assert_eq!(edited.to_string(), model);
        for at in [0, 1, 17, 2048, 7999, 8000] {
            let (a, b) = original.split_at(CharPos(at)).unwrap();
            assert_eq!(a.append(&b).to_string(), text);
        }
        let start = original.char_to_byte(CharPos(1500)).unwrap();
        let end = original.char_to_byte(CharPos(6000)).unwrap();
        let chunks = original.chunks(start..end).unwrap().collect::<String>();
        assert_eq!(chunks, text.chars().skip(1500).take(4500).collect::<String>());
        let reverse = original.chunks(start..end).unwrap().rev().collect::<Vec<_>>();
        assert_eq!(reverse.into_iter().rev().collect::<String>(), chunks);
        assert_eq!(
            original.chars_from(CharPos(7010)).unwrap().take(9).collect::<String>(),
            text.chars().skip(7010).take(9).collect::<String>()
        );
    }
    #[test]
    fn invalid_ranges_do_not_panic() {
        let rope = Rope::from("é🙂");
        assert!(rope.byte_to_char(BytePos(1)).is_err());
        assert!(rope.chunks(BytePos(0)..BytePos(3)).is_err());
        assert!(rope.replace(CharPos(2)..CharPos(1), "").is_err());
        assert!(rope.slice(CharPos(0)..CharPos(3)).is_err());
        let mut cursor = rope.cursor(BytePos(2)).unwrap();
        assert_eq!(cursor.next_char(), Some('🙂'));
        assert_eq!(cursor.previous_char(), Some('🙂'));
        assert_eq!(cursor.previous_char(), Some('é'));
        assert_eq!(cursor.previous_char(), None);
    }
    #[test]
    fn deterministic_edit_model_and_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Rope>();
        let mut rope = Rope::new();
        let mut model = String::new();
        let mut seed = 7u64;
        for _ in 0..1200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = model.chars().count();
            let a = (seed as usize) % (len + 1);
            let b = a + ((seed >> 32) as usize) % (len - a + 1);
            let insert = ["x", "é", "🙂\n", ""][((seed >> 18) % 4) as usize];
            let start = byte_at_char(&model, a);
            let end = byte_at_char(&model, b);
            model.replace_range(start..end, insert);
            rope = rope.replace(CharPos(a)..CharPos(b), insert).unwrap();
            assert_eq!(rope.to_string(), model);
            assert_eq!(rope.summary(), TextSummary::of(&model));
        }
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn shared_doubling_rejects_overflow_without_flattening() {
        let mut rope = Rope::from("x");
        for _ in 0..usize::BITS - 1 {
            rope = rope.checked_append(&rope).unwrap();
        }
        let before = rope.len_bytes();
        assert!(matches!(rope.checked_append(&rope), Err(TextError::CapacityOverflow)));
        assert_eq!(rope.len_bytes(), before);
        assert_eq!(rope.char_at(CharPos(before - 1)), Some('x'));
        assert!(std::panic::catch_unwind(|| rope.append(&rope)).is_err());
    }
}
