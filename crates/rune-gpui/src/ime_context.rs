//! Owner-prepared bounded context; never scans a live document from platform
//! callbacks. Numeric origins must come from the owner's verified Unicode index.
use crate::composition::CompositionContext;
use std::{ops::Range, sync::Arc};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImeIdentity {
    pub window_id: u64,
    pub buffer_id: u64,
    pub publication_epoch: u64,
    pub owner_sequence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextError {
    TooLarge,
    Overflow,
    Stale,
    OutsidePrepared,
    NotScalarBoundary,
    InvalidRange,
}
#[derive(Clone)]
pub struct PreparedImeContext {
    identity: ImeIdentity,
    text: Arc<str>,
    scalar_start: usize,
    utf16_start: usize,
    // local UTF-16 offset and UTF-8 byte offset at each scalar boundary.
    boundaries: Vec<(usize, usize)>,
}
impl PreparedImeContext {
    pub fn identity(&self) -> ImeIdentity {
        self.identity
    }
    pub fn scalar_start(&self) -> usize {
        self.scalar_start
    }
    pub fn utf16_start(&self) -> usize {
        self.utf16_start
    }
    pub fn utf16_range(&self) -> Range<usize> {
        self.utf16_start..self.utf16_start + self.boundaries.last().unwrap().0
    }
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn new(
        identity: ImeIdentity,
        scalar_start: usize,
        utf16_start: usize,
        text: Arc<str>,
    ) -> Result<Self, ContextError> {
        if text.len() > 65_536 {
            return Err(ContextError::TooLarge);
        }
        let mut boundaries = vec![(0, 0)];
        let mut utf16 = 0usize;
        for (byte, ch) in text.char_indices() {
            utf16 = utf16.checked_add(ch.len_utf16()).ok_or(ContextError::Overflow)?;
            if utf16 > 8192 {
                return Err(ContextError::TooLarge);
            }
            boundaries.push((utf16, byte + ch.len_utf8()));
        }
        scalar_start.checked_add(boundaries.len() - 1).ok_or(ContextError::Overflow)?;
        utf16_start.checked_add(utf16).ok_or(ContextError::Overflow)?;
        Ok(Self { identity, text, scalar_start, utf16_start, boundaries })
    }
    fn local(
        &self,
        identity: ImeIdentity,
        range: Range<usize>,
    ) -> Result<Range<usize>, ContextError> {
        if identity != self.identity {
            return Err(ContextError::Stale);
        }
        if range.start > range.end {
            return Err(ContextError::InvalidRange);
        }
        let start =
            range.start.checked_sub(self.utf16_start).ok_or(ContextError::OutsidePrepared)?;
        let end = range.end.checked_sub(self.utf16_start).ok_or(ContextError::OutsidePrepared)?;
        if end > self.boundaries.last().unwrap().0 {
            return Err(ContextError::OutsidePrepared);
        }
        Ok(start..end)
    }
    pub fn exact_scalar_range(
        &self,
        identity: ImeIdentity,
        range: Range<usize>,
    ) -> Result<Range<usize>, ContextError> {
        let range = self.local(identity, range)?;
        let start = self
            .boundaries
            .binary_search_by_key(&range.start, |b| b.0)
            .map_err(|_| ContextError::NotScalarBoundary)?;
        let end = self
            .boundaries
            .binary_search_by_key(&range.end, |b| b.0)
            .map_err(|_| ContextError::NotScalarBoundary)?;
        Ok(self.scalar_start + start..self.scalar_start + end)
    }
    pub fn replacement_context(
        &self,
        identity: ImeIdentity,
        range: Range<usize>,
    ) -> Result<CompositionContext, ContextError> {
        Ok(CompositionContext {
            window_id: identity.window_id,
            buffer_id: identity.buffer_id,
            publication_epoch: identity.publication_epoch,
            owner_sequence: identity.owner_sequence,
            replacement: self.exact_scalar_range(identity, range)?,
        })
    }
    /// Reads may expand a surrogate interior and return their adjusted global
    /// range. Replacements must instead use exact_scalar_range and reject it.
    pub fn read_adjusted(
        &self,
        identity: ImeIdentity,
        range: Range<usize>,
    ) -> Result<(Range<usize>, String), ContextError> {
        let r = self.local(identity, range)?;
        let start = match self.boundaries.binary_search_by_key(&r.start, |b| b.0) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let end = match self.boundaries.binary_search_by_key(&r.end, |b| b.0) {
            Ok(i) | Err(i) => i,
        };
        let (u0, b0) = self.boundaries[start];
        let (u1, b1) = self.boundaries[end];
        Ok((self.utf16_start + u0..self.utf16_start + u1, self.text[b0..b1].to_owned()))
    }
    pub fn scalar_to_utf16(
        &self,
        identity: ImeIdentity,
        position: usize,
    ) -> Result<usize, ContextError> {
        if identity != self.identity {
            return Err(ContextError::Stale);
        }
        let local = position.checked_sub(self.scalar_start).ok_or(ContextError::OutsidePrepared)?;
        let &(u, _) = self.boundaries.get(local).ok_or(ContextError::OutsidePrepared)?;
        Ok(self.utf16_start + u)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> ImeIdentity {
        ImeIdentity { window_id: 1, buffer_id: 2, publication_epoch: 3, owner_sequence: 4 }
    }
    #[test]
    fn emoji_replacement_exact_and_read_adjusted_are_distinct() {
        let c = PreparedImeContext::new(id(), 10, 20, Arc::from("é🙂x")).unwrap();
        assert_eq!(c.exact_scalar_range(id(), 21..23), Ok(11..12));
        assert_eq!(c.exact_scalar_range(id(), 22..23), Err(ContextError::NotScalarBoundary));
        assert_eq!(c.read_adjusted(id(), 22..22), Ok((21..23, "🙂".into())));
        assert_eq!(c.scalar_to_utf16(id(), 12), Ok(23));
    }
    #[test]
    fn stale_outside_and_oversize_contexts_are_rejected() {
        let c = PreparedImeContext::new(id(), 0, 0, Arc::from("x")).unwrap();
        let mut stale = id();
        stale.publication_epoch += 1;
        assert_eq!(c.exact_scalar_range(stale, 0..1), Err(ContextError::Stale));
        assert_eq!(c.exact_scalar_range(id(), 0..2), Err(ContextError::OutsidePrepared));
        assert!(matches!(
            PreparedImeContext::new(id(), usize::MAX, 0, Arc::from("x")),
            Err(ContextError::Overflow)
        ));
        assert!(matches!(
            PreparedImeContext::new(id(), 0, 0, Arc::from("x".repeat(8193))),
            Err(ContextError::TooLarge)
        ));
    }
}
