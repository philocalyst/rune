use std::sync::Arc;
/// Zero-based Unicode-scalar positions. Implement on an immutable rope snapshot,
/// not a live buffer/GC guard. Construction of the iterator must seek directly,
/// rather than scan/copy the prefix. next_line_start must use the newline index.
pub trait TextSnapshot: Send + Sync {
    fn revision(&self) -> u64;
    fn len_chars(&self) -> usize;
    fn chars_from(&self, position: usize) -> Box<dyn Iterator<Item = char> + '_>;
    fn next_line_start(&self, position: usize) -> Option<usize>;
}
/// Standalone adapter/fixture. Snapshot creation is O(n), iteration seeks within
/// at most 256 scalars; clones share text and indices. Rope adapters avoid this
/// one-time full scan and use their existing summary indices instead.
#[derive(Clone)]
pub struct StringSnapshot {
    text: Arc<str>,
    checkpoints: Arc<[usize]>,
    lines: Arc<[usize]>,
    len: usize,
    revision: u64,
}
impl StringSnapshot {
    pub fn new(text: impl Into<Arc<str>>, revision: u64) -> Self {
        let text = text.into();
        let mut checkpoints = Vec::new();
        let mut lines = Vec::new();
        let mut len = 0;
        for (index, (byte, ch)) in text.char_indices().enumerate() {
            if index % 256 == 0 {
                checkpoints.push(byte);
            }
            if ch == '\n' {
                lines.push(index + 1);
            }
            len += 1;
        }
        Self { text, checkpoints: checkpoints.into(), lines: lines.into(), len, revision }
    }
    fn byte(&self, position: usize) -> usize {
        if position >= self.len {
            return self.text.len();
        }
        let checkpoint = position / 256;
        self.text[self.checkpoints[checkpoint]..]
            .char_indices()
            .nth(position % 256)
            .map(|(byte, _)| byte + self.checkpoints[checkpoint])
            .unwrap_or(self.text.len())
    }
}
impl TextSnapshot for StringSnapshot {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_chars(&self) -> usize {
        self.len
    }
    fn chars_from(&self, position: usize) -> Box<dyn Iterator<Item = char> + '_> {
        Box::new(self.text[self.byte(position)..].chars())
    }
    fn next_line_start(&self, position: usize) -> Option<usize> {
        self.lines.get(self.lines.partition_point(|line| *line <= position)).copied()
    }
}
