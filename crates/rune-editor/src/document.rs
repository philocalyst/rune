use std::{fmt, ops::Range, sync::Arc};

/// Immutable, character-indexed text roots. Replacements must leave `self`
/// unchanged on failure. Persistent text implementations make clone O(1).
/// No Lisp values, runtime locks, or mutable renderer references cross here.
pub trait Document: Clone {
    type Error: fmt::Display;
    fn empty() -> Self;
    fn len_chars(&self) -> usize;
    fn replace(&self, range: Range<usize>, text: &str) -> Result<Self, Self::Error>;
    fn visit_chars(&self, start: usize, max: usize, visitor: &mut dyn FnMut(usize, char) -> bool);

    /// Explicit materialization for diagnostics/files, not the rendering path.
    fn to_text(&self) -> String {
        let mut out = String::new();
        self.visit_chars(0, self.len_chars(), &mut |_, ch| {
            out.push(ch);
            true
        });
        out
    }
}

/// Reference backend for the executable headless session and tests. Production
/// uses a rune-text Rope adapter; this backend intentionally makes edits O(n).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StringDocument(Arc<str>);

impl From<&str> for StringDocument {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}
impl From<String> for StringDocument {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRange;
impl fmt::Display for InvalidRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid character range")
    }
}
impl std::error::Error for InvalidRange {}

impl Document for StringDocument {
    type Error = InvalidRange;
    fn empty() -> Self {
        Self::default()
    }
    fn len_chars(&self) -> usize {
        self.0.chars().count()
    }
    fn replace(&self, range: Range<usize>, text: &str) -> Result<Self, Self::Error> {
        if range.start > range.end || range.end > self.len_chars() {
            return Err(InvalidRange);
        }
        let byte = |position| self.0.char_indices().nth(position).map_or(self.0.len(), |(i, _)| i);
        let mut out = String::with_capacity(self.0.len().saturating_add(text.len()));
        out.push_str(&self.0[..byte(range.start)]);
        out.push_str(text);
        out.push_str(&self.0[byte(range.end)..]);
        Ok(Self::from(out))
    }
    fn visit_chars(&self, start: usize, max: usize, visitor: &mut dyn FnMut(usize, char) -> bool) {
        for (offset, ch) in self.0.chars().skip(start).take(max).enumerate() {
            if !visitor(start + offset, ch) {
                break;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Left,
    Right,
}

/// Mapping of external view anchors through one validated text splice. This is
/// view bookkeeping, not a replacement for rune-text's marker/property model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub end: usize,
    pub inserted: usize,
}
impl Splice {
    pub fn map(self, position: usize, bias: Bias) -> crate::Result<usize> {
        if self.start > self.end {
            return Err(crate::EditorError::InvalidRange);
        }
        let after =
            self.start.checked_add(self.inserted).ok_or(crate::EditorError::InvalidRange)?;
        if position < self.start {
            Ok(position)
        } else if position > self.end {
            position
                .checked_sub(self.end - self.start)
                .and_then(|value| value.checked_add(self.inserted))
                .ok_or(crate::EditorError::InvalidPosition)
        } else if position == self.end && self.end > self.start {
            Ok(after)
        } else {
            Ok(if bias == Bias::Right { after } else { self.start })
        }
    }
}
