//! Bounded UI-local evaluator draft. Never a runtime buffer or source authority.
use std::ops::Range;
pub const MAX_SOURCE_BYTES: usize = 65_536;
#[derive(Clone, Debug)]
pub struct Submission {
    pub id: u64,
    pub source: String,
    revision: u64,
}
pub struct CommandDraft {
    text: String,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    confirmation_fence: bool,
    base: Option<(String, Range<usize>)>,
    revision: u64,
    pending: Option<u64>,
    next_id: u64,
    last: String,
}
impl Default for CommandDraft {
    fn default() -> Self {
        Self {
            text: String::new(),
            selection: 0..0,
            marked: None,
            confirmation_fence: false,
            base: None,
            revision: 0,
            pending: None,
            next_id: 64,
            last: String::new(),
        }
    }
}
impl CommandDraft {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }
    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }
    pub fn pending(&self) -> Option<u64> {
        self.pending
    }
    pub fn composing(&self) -> bool {
        self.marked.is_some()
    }
    pub fn utf16_len(&self) -> usize {
        self.text.encode_utf16().count()
    }
    pub fn byte(&self, p: usize) -> Result<usize, &'static str> {
        byte(&self.text, p)
    }
    pub fn utf16(&self, p: usize) -> Option<usize> {
        self.text.get(..p).map(|s| s.encode_utf16().count())
    }
    pub fn read(&self, range: Range<usize>) -> Option<(Range<usize>, String)> {
        if range.start > range.end || range.end > self.utf16_len() {
            return None;
        }
        let mut a = range.start;
        let mut b = range.end;
        if self.byte(a).is_err() {
            a = a.checked_sub(1)?;
        }
        if self.byte(b).is_err() {
            b += 1;
        }
        Some((a..b, self.text.get(self.byte(a).ok()?..self.byte(b).ok()?)?.into()))
    }
    pub fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        preedit: Option<Option<Range<usize>>>,
    ) -> Result<(), &'static str> {
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection.clone());
        if range.start > range.end {
            return Err("reversed text range");
        }
        let a = self.byte(range.start)?;
        let b = self.byte(range.end)?;
        if self.text.len() - (b - a) + text.len() > MAX_SOURCE_BYTES {
            return Err("source exceeds 65536 bytes; draft retained");
        }
        let revision = self.revision.checked_add(1).ok_or("draft revision exhausted")?;
        let count = text.encode_utf16().count();
        let selection = if let Some(Some(r)) = &preedit {
            if r.start > r.end {
                return Err("reversed preedit selection");
            }
            byte(text, r.start)?;
            byte(text, r.end)?;
            range.start + r.start..range.start + r.end
        } else {
            range.start + count..range.start + count
        };
        let mut next = self.text[..a].to_owned();
        next.push_str(text);
        next.push_str(&self.text[b..]);
        if preedit.is_some() && self.base.is_none() {
            self.base = Some((self.text.clone(), self.selection.clone()));
        }
        if preedit.is_none() && self.marked.is_some() {
            self.confirmation_fence = true;
        }
        self.text = next;
        self.selection = selection;
        self.revision = revision;
        self.marked = preedit.as_ref().map(|_| range.start..range.start + count);
        if preedit.is_none() {
            self.base = None;
        }
        Ok(())
    }
    pub fn unmark(&mut self) {
        if self.marked.is_some() {
            self.confirmation_fence = true;
        }
        self.marked = None;
        self.base = None;
    }
    pub fn release_confirmation(&mut self) {
        if !self.composing() {
            self.confirmation_fence = false;
        }
    }
    pub fn cancel_preedit(&mut self) {
        self.confirmation_fence = false;
        if let Some((text, selection)) = self.base.take() {
            self.text = text;
            self.selection = selection;
        }
        self.marked = None;
    }
    pub fn select(&mut self, range: Range<usize>) -> Result<(), &'static str> {
        if range.start > range.end {
            return Err("reversed selection");
        }
        self.byte(range.start)?;
        self.byte(range.end)?;
        self.selection = range;
        Ok(())
    }
    pub fn move_cursor(&mut self, direction: i32) {
        let p = if direction < 0 { self.selection.start } else { self.selection.end };
        let p = if self.selection.is_empty() {
            let b = self.byte(p).unwrap();
            if direction < 0 {
                self.text[..b].chars().next_back().map_or(p, |c| p - c.len_utf16())
            } else {
                self.text[b..].chars().next().map_or(p, |c| p + c.len_utf16())
            }
        } else {
            p
        };
        self.selection = p..p;
    }
    pub fn delete(&mut self, backward: bool) -> Result<(), &'static str> {
        let selected = self.selection.clone();
        if selected.is_empty() {
            self.move_cursor(if backward { -1 } else { 1 });
            let p = self.selection.start;
            self.selection = if backward { p..selected.end } else { selected.start..p };
        }
        self.replace(None, "", None)
    }
    pub fn prepare(&mut self) -> Result<Submission, &'static str> {
        if self.composing() {
            return Err("confirm composition before evaluating");
        }
        if self.confirmation_fence {
            return Err("release the IME confirmation key before evaluating");
        }
        if self.pending.is_some() {
            return Err("evaluation pending; not resubmitted");
        }
        if self.text.trim().is_empty() {
            return Err("enter Lisp source first");
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or("request IDs exhausted")?;
        Ok(Submission { id, source: self.text.clone(), revision: self.revision })
    }
    /// Called only after definite FIFO admission. Never automatically retry.
    pub fn admitted(&mut self, submission: Submission) {
        self.pending = Some(submission.id);
        self.last = submission.source;
        if self.revision == submission.revision {
            self.text.clear();
            self.selection = 0..0;
            self.unmark();
        }
    }
    /// A definite rejection is delivered asynchronously by the owner-view's
    /// mandatory callback. Retain the draft and gate further attempts until that
    /// terminal UI notice is consumed, bounding the outcome mailbox as well.
    pub fn rejected_attempt(&mut self, id: u64) {
        self.pending = Some(id);
    }
    pub fn complete(&mut self, id: u64) -> bool {
        if self.pending == Some(id) {
            self.pending = None;
            true
        } else {
            false
        }
    }
    pub fn recall(&mut self) -> Result<(), &'static str> {
        if self.pending.is_some() {
            return Err("evaluation still pending");
        }
        if self.composing() {
            return Err("confirm composition first");
        }
        if self.last.is_empty() {
            return Err("no manual submission to recall");
        }
        let last = self.last.clone();
        self.replace(Some(0..self.utf16_len()), &last, None)
    }
}
fn byte(text: &str, pos: usize) -> Result<usize, &'static str> {
    let mut n = 0;
    for (b, c) in text.char_indices() {
        if n == pos {
            return Ok(b);
        }
        n += c.len_utf16();
        if n > pos {
            return Err("UTF-16 surrogate interior");
        }
    }
    if n == pos { Ok(text.len()) } else { Err("text range unavailable") }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_utf16_rejects_surrogate_and_atomic_bad_selection() {
        let mut d = CommandDraft::new();
        d.replace(None, "a🙂z", None).unwrap();
        let old = d.text().to_owned();
        assert!(d.replace(Some(2..3), "x", None).is_err());
        assert_eq!(d.text(), old);
        assert_eq!(d.read(2..2), Some((1..3, "🙂".into())));
        assert!(d.replace(Some(1..3), "🙂", Some(Some(1..1))).is_err());
        assert_eq!(d.text(), old);
    }
    #[test]
    fn preedit_cannot_submit_and_cancel_restores_original() {
        let mut d = CommandDraft::new();
        d.replace(None, "abc", None).unwrap();
        d.select(1..2).unwrap();
        d.replace(None, "🙂", Some(None)).unwrap();
        assert!(d.prepare().is_err());
        d.cancel_preedit();
        assert_eq!(d.text(), "abc");
        assert_eq!(d.selection(), 1..2);
    }
    #[test]
    fn only_admitted_source_clears_and_matching_outcome_unblocks() {
        let mut d = CommandDraft::new();
        d.replace(None, "(insert \"λ\")", None).unwrap();
        let s = d.prepare().unwrap();
        assert_eq!(d.text(), s.source);
        d.admitted(s.clone());
        assert!(d.text().is_empty());
        assert!(d.prepare().is_err());
        d.replace(None, "next", None).unwrap();
        assert!(!d.complete(s.id + 1));
        assert!(d.pending().is_some());
        assert!(d.complete(s.id));
        assert_eq!(d.text(), "next");
        assert!(d.prepare().is_ok());
    }
    #[test]
    fn final_ime_commit_then_enter_is_single_pending_request() {
        let mut d = CommandDraft::new();
        d.replace(None, "候", Some(None)).unwrap();
        d.replace(None, "候補", None).unwrap();
        assert!(!d.composing());
        assert!(d.prepare().is_err()); // Same confirmation key event cannot run source.
        d.release_confirmation();
        let s = d.prepare().unwrap();
        d.admitted(s);
        assert!(d.prepare().is_err());
        assert!(d.recall().is_err());
    }
    #[test]
    fn cap_is_bytes_and_empty_sources_not_submitted() {
        let mut d = CommandDraft::new();
        assert!(d.prepare().is_err());
        d.replace(None, &"x".repeat(MAX_SOURCE_BYTES), None).unwrap();
        assert!(d.replace(None, "λ", None).is_err());
        assert_eq!(d.text().len(), MAX_SOURCE_BYTES);
    }
    #[test]
    fn navigation_and_delete_do_not_split_surrogates() {
        let mut d = CommandDraft::new();
        d.replace(None, "a🙂", None).unwrap();
        d.move_cursor(-1);
        assert_eq!(d.selection(), 1..1);
        d.delete(false).unwrap();
        assert_eq!(d.text(), "a");
    }
    #[test]
    fn late_rejection_cannot_complete_later_admitted_retry() {
        let mut draft = CommandDraft::default();
        draft.replace(None, "(insert \"x\")", None).unwrap();
        let rejected = draft.prepare().unwrap(); // Not admitted; callback can arrive later.
        let accepted = draft.prepare().unwrap();
        assert!(accepted.id > rejected.id);
        draft.admitted(accepted.clone());
        assert!(!draft.complete(rejected.id));
        assert_eq!(draft.pending(), Some(accepted.id));
        assert!(draft.complete(accepted.id));
    }
    #[test]
    fn recall_requires_actual_submission_and_never_overwrites_on_absence() {
        let mut draft = CommandDraft::new();
        draft.replace(None, "unfinished", None).unwrap();
        assert!(draft.recall().is_err());
        assert_eq!(draft.text(), "unfinished");
        let submission = draft.prepare().unwrap();
        draft.admitted(submission.clone());
        draft.complete(submission.id);
        draft.recall().unwrap();
        assert_eq!(draft.text(), "unfinished");
    }
    #[test]
    fn rejected_attempt_keeps_draft_and_bounds_unobserved_attempts() {
        let mut draft = CommandDraft::new();
        draft.replace(None, "source", None).unwrap();
        let s = draft.prepare().unwrap();
        draft.rejected_attempt(s.id);
        assert!(draft.prepare().is_err());
        assert_eq!(draft.text(), "source");
        assert!(draft.complete(s.id));
        assert!(draft.prepare().unwrap().id > s.id);
    }
}
