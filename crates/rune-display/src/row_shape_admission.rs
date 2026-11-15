//! Bounded structural admission of producer-supplied contextual glyph positions.
//!
//! This module does not authenticate a font key or establish that glyphs were
//! shaped. Coverage verdicts and positions must come from a trusted producer.
use crate::ContextualClusterWidth;
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_ROW_UTF8_BYTES: usize = 256;
pub const MAX_ROW_SCALARS: usize = 64;
pub const MAX_ROW_GLYPHS: usize = 64;
const MAX_SUPPORTED_CODEPOINT: u32 = 0x036f;

/// A producer's coverage verdict, not an inference from scalar advance widths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlyphCoverage {
    VerifiedAvailable,
    Missing,
    #[default]
    Unknown,
}

/// Plain measurement input. `font_key` is meaningful only in its producer domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    pub font_key: u64,
    pub coverage: GlyphCoverage,
    pub byte: usize,
    pub x: f32,
    pub y: f32,
    pub emoji: bool,
}

/// An exact complete-source boundary, including the start and end of the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphemeBoundary {
    pub byte_offset: usize,
    pub scalar_offset: usize,
}

/// Enumerate complete-text grapheme boundaries within the first supported subset.
///
/// The character policy is non-control codepoints at or below U+036F, not the
/// Unicode printable-character category. In particular, some format characters
/// are in this range; their glyph mapping must still pass admission separately.
pub fn grapheme_boundaries(text: &str) -> Result<Vec<GraphemeBoundary>, &'static str> {
    if text.is_empty() || text.len() > MAX_ROW_UTF8_BYTES {
        return Err("contextual row byte budget");
    }
    let scalar_count = text.chars().take(MAX_ROW_SCALARS + 1).count();
    if scalar_count > MAX_ROW_SCALARS {
        return Err("contextual row scalar budget");
    }
    if text.chars().any(|ch| ch.is_control() || u32::from(ch) > MAX_SUPPORTED_CODEPOINT) {
        return Err("contextual row character policy unsupported");
    }

    let mut boundaries = Vec::with_capacity(scalar_count + 1);
    let mut scalar_offset = 0;
    for (byte_offset, grapheme) in text.grapheme_indices(true) {
        boundaries.push(GraphemeBoundary { byte_offset, scalar_offset });
        scalar_offset += grapheme.chars().count();
    }
    boundaries.push(GraphemeBoundary { byte_offset: text.len(), scalar_offset });
    Ok(boundaries)
}

fn validate_row_width(width: f32, max_width: f32) -> Result<(), &'static str> {
    if !width.is_finite() || width <= 0.0 || !max_width.is_finite() || max_width <= 0.0 {
        return Err("invalid contextual row width");
    }
    if width > max_width {
        return Err("contextual row exceeds available width");
    }
    Ok(())
}

fn validate_glyph(
    glyph: &PositionedGlyph,
    expected_font_key: u64,
    expected_byte: usize,
    first: bool,
) -> Result<(), &'static str> {
    if glyph.font_key != expected_font_key {
        return Err("contextual fallback font unsupported");
    }
    match glyph.coverage {
        GlyphCoverage::VerifiedAvailable => {}
        GlyphCoverage::Missing => return Err("contextual glyph missing"),
        GlyphCoverage::Unknown => return Err("contextual glyph coverage unknown"),
    }
    if glyph.emoji || glyph.byte != expected_byte {
        return Err("contextual glyph mapping unsupported");
    }
    if !glyph.x.is_finite() || glyph.x < 0.0 || !glyph.y.is_finite() || glyph.y != 0.0 {
        return Err("contextual glyph offset unsupported");
    }
    if first && glyph.x != 0.0 {
        return Err("contextual row leading offset unsupported");
    }
    Ok(())
}

/// Admit exactly one glyph for each extended grapheme of a complete source row.
///
/// Unsupported mappings fail rather than splitting a ligature, moving a source
/// offset, or substituting an invented width. Absolute-origin numerical safety
/// remains the responsibility of the measured owner-layout consumer.
pub fn admit_positioned_row(
    text: &str,
    expected_font_key: u64,
    glyphs: &[PositionedGlyph],
    width: f32,
    max_width: f32,
) -> Result<Vec<ContextualClusterWidth>, &'static str> {
    if glyphs.len() > MAX_ROW_GLYPHS {
        return Err("contextual row glyph budget");
    }
    let boundaries = grapheme_boundaries(text)?;
    if glyphs.len() != boundaries.len() - 1 {
        return Err("contextual row glyph coverage mismatch");
    }
    validate_row_width(width, max_width)?;

    for (index, glyph) in glyphs.iter().enumerate() {
        validate_glyph(glyph, expected_font_key, boundaries[index].byte_offset, index == 0)?;
    }

    let mut clusters = Vec::with_capacity(glyphs.len());
    for (index, glyph) in glyphs.iter().enumerate() {
        let next_x = glyphs.get(index + 1).map_or(width, |next| next.x);
        let advance = next_x - glyph.x;
        if !next_x.is_finite() || !advance.is_finite() || advance <= 0.0 || next_x > width {
            return Err("contextual glyph progress unsupported");
        }
        clusters.push(ContextualClusterWidth {
            source: boundaries[index].scalar_offset..boundaries[index + 1].scalar_offset,
            width: advance,
        });
    }
    Ok(clusters)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT_KEY: u64 = 7;
    const ROW_WIDTH: f32 = 20.0;
    const AVAILABLE_WIDTH: f32 = 24.0;

    fn glyph(byte: usize, x: f32) -> PositionedGlyph {
        PositionedGlyph {
            font_key: FONT_KEY,
            coverage: GlyphCoverage::VerifiedAvailable,
            byte,
            x,
            y: 0.0,
            emoji: false,
        }
    }

    #[test]
    fn boundaries_preserve_utf8_and_scalar_offsets() {
        assert_eq!(
            grapheme_boundaries("a\u{301}b").unwrap(),
            vec![
                GraphemeBoundary { byte_offset: 0, scalar_offset: 0 },
                GraphemeBoundary { byte_offset: 3, scalar_offset: 2 },
                GraphemeBoundary { byte_offset: 4, scalar_offset: 3 },
            ]
        );
    }

    #[test]
    fn admitted_advances_preserve_exact_source_and_input() {
        let glyphs = [glyph(0, 0.0), glyph(3, 9.5)];
        let original = glyphs;
        let clusters = admit_positioned_row("a\u{301}b", FONT_KEY, &glyphs, ROW_WIDTH, AVAILABLE_WIDTH)
            .unwrap();
        assert_eq!(clusters[0], ContextualClusterWidth { source: 0..2, width: 9.5 });
        assert_eq!(clusters[1], ContextualClusterWidth { source: 2..3, width: 10.5 });
        assert_eq!(clusters.iter().map(|cluster| cluster.width).sum::<f32>(), ROW_WIDTH);
        assert_eq!(glyphs, original);
    }

    #[test]
    fn missing_and_default_unknown_coverage_fail_closed() {
        assert_eq!(GlyphCoverage::default(), GlyphCoverage::Unknown);
        for coverage in [GlyphCoverage::Missing, GlyphCoverage::Unknown] {
            let input = PositionedGlyph { coverage, ..glyph(0, 0.0) };
            assert!(admit_positioned_row("x", FONT_KEY, &[input], 10.0, AVAILABLE_WIDTH).is_err());
        }
    }

    #[test]
    fn fallback_emoji_and_wrong_source_byte_are_rejected() {
        let inputs = [
            PositionedGlyph { font_key: FONT_KEY + 1, ..glyph(0, 0.0) },
            PositionedGlyph { emoji: true, ..glyph(0, 0.0) },
            PositionedGlyph { byte: 1, ..glyph(0, 0.0) },
        ];
        for input in inputs {
            assert!(admit_positioned_row("x", FONT_KEY, &[input], 10.0, AVAILABLE_WIDTH).is_err());
        }
        assert!(admit_positioned_row(
            "a\u{301}b", FONT_KEY, &[glyph(0, 0.0), glyph(1, 10.0)], ROW_WIDTH, AVAILABLE_WIDTH
        ).is_err());
    }

    #[test]
    fn ligatures_and_multi_glyph_graphemes_are_not_split() {
        assert!(admit_positioned_row("ffi", FONT_KEY, &[glyph(0, 0.0)], ROW_WIDTH, AVAILABLE_WIDTH)
            .is_err());
        assert!(admit_positioned_row(
            "a\u{301}", FONT_KEY, &[glyph(0, 0.0), glyph(0, 10.0)], ROW_WIDTH, AVAILABLE_WIDTH
        ).is_err());
    }

    #[test]
    fn nonfinite_negative_and_nonzero_offsets_are_rejected() {
        for x in [f32::NAN, f32::INFINITY, -1.0, 0.5] {
            assert!(admit_positioned_row("x", FONT_KEY, &[glyph(0, x)], 10.0, AVAILABLE_WIDTH)
                .is_err());
        }
        for y in [f32::NAN, f32::INFINITY, -1.0, 1.0] {
            let input = PositionedGlyph { y, ..glyph(0, 0.0) };
            assert!(admit_positioned_row("x", FONT_KEY, &[input], 10.0, AVAILABLE_WIDTH).is_err());
        }
    }

    #[test]
    fn non_progress_and_positions_beyond_row_width_are_rejected() {
        for second_x in [0.0, 30.0] {
            let inputs = [glyph(0, 0.0), glyph(1, second_x)];
            assert!(admit_positioned_row("ab", FONT_KEY, &inputs, ROW_WIDTH, AVAILABLE_WIDTH)
                .is_err());
        }
        // The producer may already have lost a small advance to f32 absorption.
        let large = 1e20_f32;
        let absorbed_next = large + 1.0;
        assert_eq!(large, absorbed_next);
        let absorbed = [glyph(0, 0.0), glyph(1, large), glyph(2, absorbed_next)];
        assert!(admit_positioned_row("abc", FONT_KEY, &absorbed, 2e20, 2e20).is_err());
    }

    #[test]
    fn invalid_and_oversized_row_widths_are_rejected() {
        for width in [f32::NAN, f32::INFINITY, 0.0, -1.0, AVAILABLE_WIDTH + 1.0] {
            assert!(admit_positioned_row("x", FONT_KEY, &[glyph(0, 0.0)], width, AVAILABLE_WIDTH)
                .is_err());
        }
        for available in [f32::NAN, f32::INFINITY, 0.0, -1.0] {
            assert!(admit_positioned_row("x", FONT_KEY, &[glyph(0, 0.0)], 10.0, available).is_err());
        }
    }

    #[test]
    fn source_and_glyph_limits_are_explicit_and_enforced() {
        assert!(grapheme_boundaries("").is_err());
        assert!(grapheme_boundaries(&"x".repeat(MAX_ROW_UTF8_BYTES + 1)).is_err());
        assert!(grapheme_boundaries(&"x".repeat(MAX_ROW_SCALARS + 1)).is_err());
        assert!(grapheme_boundaries(&"x".repeat(MAX_ROW_SCALARS)).is_ok());
        assert!(grapheme_boundaries("\t").is_err());
        assert!(grapheme_boundaries("\u{370}").is_err());
        // The policy is not a printable-character category: soft hyphen is in range.
        assert!(grapheme_boundaries("\u{ad}").is_ok());
        let too_many = vec![glyph(0, 0.0); MAX_ROW_GLYPHS + 1];
        assert!(admit_positioned_row("x", FONT_KEY, &too_many, 10.0, AVAILABLE_WIDTH).is_err());
    }

    #[test]
    fn maximum_source_and_glyph_counts_are_admitted() {
        let text = "x".repeat(MAX_ROW_SCALARS);
        let inputs: Vec<_> = (0..MAX_ROW_GLYPHS).map(|index| glyph(index, index as f32)).collect();
        let width = MAX_ROW_GLYPHS as f32;
        let clusters = admit_positioned_row(&text, FONT_KEY, &inputs, width, width).unwrap();
        assert_eq!(clusters.len(), MAX_ROW_GLYPHS);
        assert_eq!(clusters.last().unwrap().source, MAX_ROW_SCALARS - 1..MAX_ROW_SCALARS);
    }

    #[test]
    fn zero_font_key_is_opaque_correlation_not_authentication() {
        let input = PositionedGlyph { font_key: 0, ..glyph(0, 0.0) };
        assert!(admit_positioned_row("x", 0, &[input], 10.0, AVAILABLE_WIDTH).is_ok());
    }
}
