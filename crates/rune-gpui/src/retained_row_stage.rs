//! Opt-in UI staging only. No paint API or owner-layout admission is provided.
use crate::{NativeFont, NativeFontVariantBank};
use gpui::{AnyWindowHandle, App, Hsla, ShapedLine, TextRun, TextSystem, Window};
use rune_display::{
    MeasuredFontVariant,
    retained_row_handoff::RetainedRowReply,
    retained_row_transaction::{AcceptedRowToken, RetainedRowRequest},
};
use std::{rc::Rc, sync::{Arc, Weak, atomic::{AtomicU64, Ordering}}};

const MAX_STAGED_GLYPHS: usize = 256;
pub const NORMAL_SINGLE_RUN_POLICY: u64 = 1;
static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

/// Diagnostics only. Unknown must never become VerifiedAvailable implicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagedCoverage { Unknown }

#[derive(Clone, Debug)]
pub struct NativeRowObservation {
    pub reply: RetainedRowReply,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    pub glyph_count: usize,
    pub coverage: StagedCoverage,
}

/// This preflight can run headlessly. It does not invoke Window shaping.
pub fn preflight_retained_row(
    request: &RetainedRowRequest,
    baseline: &NativeFont,
    bank: &NativeFontVariantBank,
    app: &App,
) -> Result<(), &'static str> {
    bank.validate_baseline(baseline, app)?;
    if !request.is_live() {
        return Err("retained request canceled");
    }
    let capabilities = bank.capabilities(app)?;
    if request.scope().font_bank_generation != capabilities.bank_id {
        return Err("retained request belongs to another font bank");
    }
    if request.scope().shaping_policy_revision != NORMAL_SINGLE_RUN_POLICY
        || request.scope().source_start_scalar != 0
        || request.scope().window == 0 || request.scope().buffer == 0
    {
        return Err("unsupported retained row policy or source range");
    }
    // Request construction already bounds bytes/scalars and rejects controls.
    bank.get_compatible_font(capabilities.bank_id, capabilities.metrics,
        MeasuredFontVariant::Normal, app)?;
    Ok(())
}

/// Actual native line remains private and UI-owned; nothing here paints it.
pub struct StagedNativeRow {
    line: ShapedLine,
    request: RetainedRowRequest,
    bank: Rc<NativeFontVariantBank>,
    system: Weak<TextSystem>,
    window: AnyWindowHandle,
    observation: NativeRowObservation,
}

impl StagedNativeRow {
    /// Public SDK route only. This method has NOT executed without a real Window.
    pub fn shape(
        request: RetainedRowRequest,
        baseline: &NativeFont,
        bank: Rc<NativeFontVariantBank>,
        foreground: Hsla,
        window: &Window,
        app: &App,
    ) -> Result<Self, &'static str> {
        preflight_retained_row(&request, baseline, &bank, app)?;
        if !Arc::ptr_eq(app.text_system(), window.text_system()) {
            return Err("window belongs to another text system");
        }
        if ![foreground.h, foreground.s, foreground.l, foreground.a].iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
        {
            return Err("invalid retained foreground color");
        }
        let capabilities = bank.capabilities(app)?;
        let descriptor = bank.get_compatible_font(capabilities.bank_id,
            capabilities.metrics, MeasuredFontVariant::Normal, app)?.clone();
        // This is an identity in the current TextSystem, not a coverage proof.
        // The SDK may already have resolved the descriptor through its fallback stack.
        let requested_font_id = window.text_system().resolve_font(&descriptor);
        let runs = [TextRun {
            len: request.text().len(), font: descriptor, color: foreground,
            background_color: None, underline: None, strikethrough: None,
        }];
        let line = window.text_system().shape_line(
            request.text().to_owned().into(), baseline.size, &runs, None,
        );
        if line.runs.len() > MAX_STAGED_GLYPHS {
            return Err("retained shape run budget exceeded");
        }
        if !run_fonts_match(&requested_font_id, line.runs.iter().map(|run| &run.font_id)) {
            return Err("retained shape used a different run font");
        }
        let glyph_count = line.runs.iter().try_fold(0usize, |count, run| {
            count.checked_add(run.glyphs.len())
                .filter(|count| *count <= MAX_STAGED_GLYPHS)
        }).ok_or("retained shape glyph budget exceeded")?;
        if line.runs.iter().flat_map(|run| &run.glyphs).any(|glyph| {
            !is_source_glyph_index(request.text(), glyph.index)
        }) {
            return Err("retained glyph index is not a source character boundary");
        }
        if line.runs.iter().flat_map(|run| &run.glyphs).any(|glyph| {
            let x: f32 = glyph.position.x.into();
            let y: f32 = glyph.position.y.into();
            !x.is_finite() || !y.is_finite()
        }) {
            return Err("nonfinite retained glyph position");
        }
        let width: f32 = line.width.into();
        let ascent: f32 = line.ascent.into();
        let descent: f32 = line.descent.into();
        if line.text.as_ref() != request.text() || line.len() != request.text().len()
            || glyph_count > MAX_STAGED_GLYPHS
            || ![width, ascent, descent].iter().all(|value| value.is_finite())
            || width < 0.0
        {
            return Err("unsupported retained shape observation");
        }
        let ui_token = NEXT_STAGE.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |next| next.checked_add(1)).map_err(|_| "retained stage IDs exhausted")?;
        let observation = NativeRowObservation {
            reply: RetainedRowReply { key: request.key(), ui_token },
            width, ascent, descent, glyph_count, coverage: StagedCoverage::Unknown,
        };
        Ok(Self { line, request, bank, system: Arc::downgrade(app.text_system()),
            window: window.window_handle(), observation })
    }

    pub fn observation(&self) -> NativeRowObservation {
        self.observation.clone()
    }

    /// Correlation only: even true grants NO paint or owner geometry authority.
    pub fn matches_receipt(&self, receipt: &AcceptedRowToken, window: &Window, app: &App) -> bool {
        receipt.is_live() && receipt.key() == self.request.key()
            && receipt.ui_token() == self.observation.reply.ui_token
            && self.window == window.window_handle()
            && self.system.upgrade().is_some_and(|system| {
                Arc::ptr_eq(&system, app.text_system())
                    && Arc::ptr_eq(&system, window.text_system())
            })
            && self.bank.capabilities(app).is_ok_and(|capabilities| {
                capabilities.bank_id == self.request.scope().font_bank_generation
            })
            && self.line.text.as_ref() == self.request.text()
    }
}


/// Capacity-one UI retention. Cleanup runs when explicitly observed, not on a timer.
#[derive(Default)]
pub struct StagedNativeRowSlot {
    row: Option<StagedNativeRow>,
}

impl StagedNativeRowSlot {
    pub fn replace(&mut self, row: StagedNativeRow, app: &App) -> Result<(), &'static str> {
        if !row.request.is_live() || row.bank.capabilities(app).is_err() {
            return Err("staged row is canceled or belongs to an expired context");
        }
        self.row = Some(row);
        Ok(())
    }

    /// Drops the retained native line/bank when cancellation or App mismatch is observed.
    pub fn prune(&mut self, app: &App) -> bool {
        let stale = self.row.as_ref().is_some_and(|row| {
            !row.request.is_live() || row.bank.capabilities(app).is_err()
        });
        if stale {
            self.row = None;
        }
        stale
    }

    pub fn clear(&mut self) {
        self.row = None;
    }
}

/// A glyph must refer to an actual source character, not the end sentinel.
/// Multiple glyphs may share an index; ligatures need not cover every character.
/// This is a UTF-8 source check, not a grapheme or shaping correctness proof.
fn is_source_glyph_index(source: &str, index: usize) -> bool {
    index < source.len() && source.is_char_boundary(index)
}

#[cfg(test)]
mod source_index_tests {
    use super::is_source_glyph_index;

    #[test]
    fn admits_each_source_character_start_without_requiring_grapheme_starts() {
        let source = "aé\u{301}🙂";
        for (index, _) in source.char_indices() {
            assert!(is_source_glyph_index(source, index));
        }
        // The combining mark is a character start inside a grapheme.
        assert!(is_source_glyph_index(source, 3));
    }

    #[test]
    fn rejects_utf8_interiors_end_sentinels_and_unbounded_indices() {
        let source = "aé\u{301}🙂";
        for index in [2, 4, 6, 7, 8, source.len(), usize::MAX] {
            assert!(!is_source_glyph_index(source, index));
        }
        assert!(!is_source_glyph_index("", 0));
        assert!(!is_source_glyph_index("x", 1));
        assert!(is_source_glyph_index("x", 0));
    }
}

/// Relative identity only: equal IDs may still contain a missing-glyph result.
/// Repeated runs are valid; an empty iterator does not establish coverage.
fn run_fonts_match<'a, F: Eq + 'a>(
    requested: &F,
    run_fonts: impl IntoIterator<Item = &'a F>,
) -> bool {
    run_fonts.into_iter().all(|font| font == requested)
}

#[cfg(test)]
mod run_font_tests {
    use super::run_fonts_match;

    #[test]
    fn admits_repeated_requested_identity_and_no_runs() {
        assert!(run_fonts_match(&7, [&7, &7, &7]));
        assert!(run_fonts_match(&7, std::iter::empty()));
    }

    #[test]
    fn rejects_a_foreign_run_at_each_position() {
        for fonts in [[8, 7, 7], [7, 8, 7], [7, 7, 8]] {
            assert!(!run_fonts_match(&7, fonts.iter()));
        }
    }
}

/// Result of the diagnostic canvas callback, not a presentation or coverage receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticRowPaintStatus {
    NotInvoked,
    Rejected,
    PaintReturnedOk,
    PaintReturnedError,
}

impl StagedNativeRow {
    /// Dormant diagnostic: paint the exact retained line only through GPUI's canvas
    /// paint callback. This does not admit owner geometry or establish glyph coverage.
    /// The caller must supply canvas dimensions through the normal Styled API.
    pub fn diagnostic_canvas(
        self: Rc<Self>,
        receipt: AcceptedRowToken,
        line_height: f32,
        status: Rc<std::cell::Cell<DiagnosticRowPaintStatus>>,
    ) -> gpui::Canvas<()> {
        gpui::canvas(
            |_, _, _| (),
            move |bounds, (), window, app| {
                let geometry = [
                    bounds.origin.x.into(), bounds.origin.y.into(),
                    bounds.size.width.into(), bounds.size.height.into(),
                ];
                if !self.matches_receipt(&receipt, window, app)
                    || !diagnostic_paint_geometry(geometry, self.observation.width, line_height)
                    || !diagnostic_line_arithmetic(
                        [geometry[0], geometry[1]],
                        self.observation.width,
                        self.observation.ascent,
                        self.observation.descent,
                        line_height,
                        self.line.runs.iter().flat_map(|run| &run.glyphs)
                            .map(|glyph| f32::from(glyph.position.x)),
                    )
                {
                    status.set(DiagnosticRowPaintStatus::Rejected);
                    return;
                }
                // The public canvas calls this closure in its actual paint phase.
                // Intersect with the enclosing mask; do not escape parent clipping.
                let result = window.with_content_mask(
                    Some(gpui::ContentMask { bounds }),
                    |window| self.line.paint(bounds.origin, gpui::px(line_height), window, app),
                );
                status.set(if result.is_ok() {
                    DiagnosticRowPaintStatus::PaintReturnedOk
                } else {
                    // SDK errors may follow partial scene recording; no rollback claim.
                    DiagnosticRowPaintStatus::PaintReturnedError
                });
            },
        )
    }
}

/// Reject nonrepresentable rectangles and fitting dimensions before calling SDK paint.
fn diagnostic_paint_geometry(bounds: [f32; 4], row_width: f32, line_height: f32) -> bool {
    let [x, y, width, height] = bounds;
    bounds.into_iter().chain([row_width, line_height]).all(f32::is_finite)
        && width > 0.0 && height > 0.0 && row_width >= 0.0 && line_height > 0.0
        && row_width <= width && line_height <= height
        && (x + width).is_finite() && x + width > x
        && (y + height).is_finite() && y + height > y
        && (x + row_width).is_finite() && (row_width == 0.0 || x + row_width > x)
        && (y + line_height).is_finite() && y + line_height > y
}

#[cfg(test)]
mod diagnostic_paint_tests {
    use super::diagnostic_paint_geometry;

    #[test]
    fn admits_fitting_rows_and_empty_source_width() {
        assert!(diagnostic_paint_geometry([10.0, 20.0, 100.0, 30.0], 80.0, 24.0));
        assert!(diagnostic_paint_geometry([0.0, 0.0, 100.0, 30.0], 0.0, 30.0));
    }

    #[test]
    fn rejects_overflow_nonfinite_oversize_and_absorbed_progress() {
        for (bounds, width, height) in [
            ([0.0, 0.0, 100.0, 30.0], 101.0, 20.0),
            ([0.0, 0.0, 100.0, 30.0], 90.0, 31.0),
            ([0.0, 0.0, 0.0, 30.0], 0.0, 20.0),
            ([f32::NAN, 0.0, 100.0, 30.0], 80.0, 20.0),
            ([f32::MAX, 0.0, f32::MAX, 30.0], 1.0, 20.0),
            ([1e20, 0.0, 100.0, 30.0], 80.0, 20.0),
            ([0.0, 1e20, 100.0, 30.0], 80.0, 20.0),
        ] {
            assert!(!diagnostic_paint_geometry(bounds, width, height));
        }
    }
}

/// Mirror the finite arithmetic of the SDK's unwrapped, undecorated line path.
/// Does not validate font bounding boxes, scaled raster coordinates or glyph ink.
fn diagnostic_line_arithmetic(
    origin: [f32; 2],
    width: f32,
    ascent: f32,
    descent: f32,
    line_height: f32,
    glyph_x_positions: impl IntoIterator<Item = f32>,
) -> bool {
    let above = line_height - ascent;
    let remaining = above - descent;
    let padding = remaining / 2.0;
    let baseline = padding + ascent;
    if ![above, remaining, padding, baseline, origin[1] + baseline]
        .into_iter().all(f32::is_finite)
    {
        return false;
    }
    let mut previous = 0.0;
    let mut glyph_origin = origin[0];
    let mut first_origin = None;
    for position in glyph_x_positions {
        let delta = position - previous;
        glyph_origin += delta;
        if !position.is_finite() || !delta.is_finite() || !glyph_origin.is_finite() {
            return false;
        }
        first_origin.get_or_insert(glyph_origin);
        previous = position;
    }
    (first_origin.unwrap_or(origin[0]) + width).is_finite()
}

#[cfg(test)]
mod diagnostic_line_arithmetic_tests {
    use super::diagnostic_line_arithmetic as valid;

    #[test]
    fn accepts_repeated_and_nonmonotonic_positions_without_claiming_coverage() {
        assert!(valid([10.0, 20.0], 90.0, 15.0, 5.0, 24.0,
            [0.0, 20.0, 20.0, 5.0]));
        assert!(valid([0.0, 0.0], 0.0, 15.0, 5.0, 24.0, []));
    }

    #[test]
    fn rejects_each_overflowing_downstream_arithmetic_step() {
        assert!(!valid([0.0, 0.0], 10.0, -f32::MAX, 0.0, f32::MAX, []));
        assert!(!valid([0.0, 0.0], 10.0, 0.0, -f32::MAX, f32::MAX, []));
        assert!(!valid([0.0, f32::MAX], 10.0, f32::MAX, 0.0, f32::MAX, []));
        assert!(!valid([0.0, 0.0], 10.0, 15.0, 5.0, 24.0,
            [-f32::MAX, f32::MAX]));
        assert!(!valid([f32::MAX, 0.0], 10.0, 15.0, 5.0, 24.0, [f32::MAX]));
        assert!(!valid([0.0, 0.0], f32::MAX, 15.0, 5.0, 24.0, [f32::MAX]));
    }
}
