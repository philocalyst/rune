//! Opt-in whole-text font-transition preflight. This does not shape or relayout text.
use crate::MeasuredFontVariant;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphemeFontRun {
    /// Exact Unicode scalar offsets, never byte or UTF-16 offsets.
    pub source: Range<usize>,
    pub variant: MeasuredFontVariant,
}

/// Validate a complete text snapshot, not an arbitrary visible substring.
/// Whole-text context is required for regional-indicator parity and other Unicode
/// boundary rules. No caret, source, or IME offset is changed by this operation.
///
/// Limits apply before segmentation. The caller retains snapshot/epoch authority;
/// success establishes only that font changes do not split extended graphemes.
/// It does not establish font availability, glyph width, shaping or paint support.
pub fn validate_grapheme_font_runs(text: &str, runs: &[GraphemeFontRun]) -> Result<(), &'static str> {
    if text.len() > 1024 * 1024 || runs.len() > 4096 {
        return Err("grapheme font preflight budget exceeded");
    }
    let len = text.chars().count();
    if len > 100_000 {
        return Err("grapheme font scalar budget exceeded");
    }
    let mut end = 0;
    for run in runs {
        if run.source.start != end || run.source.end <= end || run.source.end > len {
            return Err("invalid grapheme font coverage");
        }
        end = run.source.end;
    }
    if end != len {
        return Err("incomplete grapheme font coverage");
    }
    let mut scalar = 0;
    let mut index = 0;
    for grapheme in text.graphemes(true) {
        let next = scalar + grapheme.chars().count();
        // A boundary between equal variants is semantically no font change.
        while index + 1 < runs.len() && runs[index].source.end < next {
            if runs[index].source.end > scalar && runs[index].variant != runs[index + 1].variant {
                return Err("font transition splits extended grapheme");
            }
            index += 1;
        }
        while index + 1 < runs.len() && runs[index].source.end == next {
            index += 1;
        }
        scalar = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(a: usize, b: usize, variant: MeasuredFontVariant) -> GraphemeFontRun {
        GraphemeFontRun { source: a..b, variant }
    }
    #[test]
    fn rejects_real_unicode_cluster_splits_without_adjusting_offsets() {
        for text in ["a\u{301}", "👩\u{200d}💻", "🇺🇸", "👍🏽", "\r\n"] {
            let len = text.chars().count();
            let runs = vec![run(0, 1, MeasuredFontVariant::Normal), run(1, len, MeasuredFontVariant::Bold)];
            let original = runs.clone();
            assert!(validate_grapheme_font_runs(text, &runs).is_err());
            assert_eq!(runs, original);
        }
    }
    #[test]
    fn regional_indicator_context_and_equal_variant_segmentation() {
        let text = "🇺🇸🇨🇦";
        assert!(validate_grapheme_font_runs(text, &[run(0,2,MeasuredFontVariant::Normal),run(2,4,MeasuredFontVariant::Bold)]).is_ok());
        assert!(validate_grapheme_font_runs(text, &[run(0,1,MeasuredFontVariant::Normal),run(1,4,MeasuredFontVariant::Normal)]).is_ok());
    }
    #[test]
    fn exact_coverage_empty_and_budget() {
        assert!(validate_grapheme_font_runs("", &[]).is_ok());
        assert!(validate_grapheme_font_runs("abc", &[run(1,3,MeasuredFontVariant::Normal)]).is_err());
        assert!(validate_grapheme_font_runs("abc", &[run(0,2,MeasuredFontVariant::Normal)]).is_err());
        assert!(validate_grapheme_font_runs(&"x".repeat(100_001), &[]).is_err());
        assert!(validate_grapheme_font_runs(&"x".repeat(1024*1024+1), &[]).is_err());
    }
    #[test]
    fn several_equal_splits_do_not_hide_later_zwj_transition() {
        let text = "👩\u{200d}❤️\u{200d}👩";
        let len = text.chars().count();
        let runs = [run(0,1,MeasuredFontVariant::Normal),run(1,2,MeasuredFontVariant::Normal),run(2,len,MeasuredFontVariant::Italic)];
        assert!(validate_grapheme_font_runs(text, &runs).is_err());
    }
    #[test]
    fn odd_regional_indicator_context_is_whole_text() {
        for text in ["🇦🇧🇨", "🇦🇧🇨🇩🇪"] {
            let len = text.chars().count();
            assert!(validate_grapheme_font_runs(text, &[run(0,2,MeasuredFontVariant::Normal),run(2,len,MeasuredFontVariant::Bold)]).is_ok());
            assert!(validate_grapheme_font_runs(text, &[run(0,1,MeasuredFontVariant::Normal),run(1,len,MeasuredFontVariant::Bold)]).is_err());
        }
    }
    #[test]
    fn malformed_ranges_and_run_budget_fail_closed() {
        for range in [0..0, 0..usize::MAX, usize::MAX..usize::MAX] {
            assert!(validate_grapheme_font_runs("x", &[GraphemeFontRun { source: range, variant: MeasuredFontVariant::Normal }]).is_err());
        }
        let runs: Vec<_> = (0..4097).map(|i|run(i,i+1,MeasuredFontVariant::Normal)).collect();
        assert!(validate_grapheme_font_runs(&"x".repeat(4097), &runs).is_err());
    }

}
