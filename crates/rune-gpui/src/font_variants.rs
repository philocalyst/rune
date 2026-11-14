//! UI-owned, bounded real GPUI face resolution. Not enabled by FrameView yet.
use super::font_bank_identity::next_bank_id;
use super::{
    NativeFont,
    font_geometry::{Measurements, validate_variant},
};
use gpui::{App, FontId, FontStyle, FontWeight};
use rune_display::{
    MeasuredFontCapabilities as NativeFontCapabilities, MeasuredFontVariant as NativeFontVariant,
};
/// Actual Font/FontId values stay UI-owned. No Lisp value or Context is stored.
/// Missing variants remain explicit errors; normal-only startup is unaffected.
pub struct NativeFontVariantBank {
    fonts: [Option<NativeFont>; 4],
    bank_id: u64,
    base_metrics: rune_display::FontMetrics,
    system: std::sync::Weak<gpui::TextSystem>,
    errors: [Option<&'static str>; 4],
    _ui_owner: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl NativeFontVariantBank {
    pub fn measure(base: &NativeFont, cx: &App) -> Result<Self, &'static str> {
        if base.font.weight != FontWeight::NORMAL || base.font.style != FontStyle::Normal {
            return Err("font variant bank requires a normal baseline face");
        }
        let size: f32 = base.size.into();
        let refreshed = NativeFont::measure(base.font.clone(), size, base.metrics.line_gap, cx)?;
        if refreshed.measured_advances != base.measured_advances
            || refreshed.metrics != base.metrics
        {
            return Err("baseline font measurements no longer match this text system");
        }
        let mut bank = Self {
            fonts: [Some(refreshed), None, None, None],
            bank_id: next_bank_id()?,
            base_metrics: base.metrics,
            system: std::sync::Arc::downgrade(cx.text_system()),
            errors: [None; 4],
            _ui_owner: std::marker::PhantomData,
        };
        let normal_id = cx.text_system().resolve_font(&base.font);
        let mut ids: [Option<FontId>; 4] = [Some(normal_id), None, None, None];
        for (slot, weight, style) in [
            (1, FontWeight::BOLD, FontStyle::Normal),
            (2, FontWeight::NORMAL, FontStyle::Italic),
            (3, FontWeight::BOLD, FontStyle::Italic),
        ] {
            // A combined variant cannot be established by falling back to an
            // unverified single style. Keep independent normal/bold available.
            if slot == 3 && (bank.fonts[1].is_none() || bank.fonts[2].is_none()) {
                bank.errors[slot] =
                    Some("combined font style requires both measured single styles");
                continue;
            }
            let mut font = base.font.clone();
            font.weight = weight;
            font.style = style;
            let id = cx.text_system().resolve_font(&font);
            if ids.iter().flatten().any(|existing| *existing == id) {
                bank.errors[slot] = Some("requested font style resolved to an existing face");
                continue;
            }
            let candidate = match NativeFont::measure(font, size, base.metrics.line_gap, cx) {
                Ok(candidate) => candidate,
                Err(error) => {
                    bank.errors[slot] = Some(error);
                    continue;
                }
            };
            let measurements = |font: &NativeFont| Measurements {
                advances: font.measured_advances,
                ascent: font.metrics.ascent,
                descent: font.metrics.descent,
            };
            // NativeFont::measure already verified its three individual ASCII
            // advances; this guard additionally checks compatibility with base.
            if let Err(error) = validate_variant(
                size,
                measurements(base),
                measurements(&candidate),
                base.metrics.line_gap,
            ) {
                bank.errors[slot] = Some(error);
                continue;
            }
            ids[slot] = Some(id);
            bank.fonts[slot] = Some(candidate);
        }
        Ok(bank)
    }
    fn validate_context(&self, cx: &App) -> Result<(), &'static str> {
        let system = self
            .system
            .upgrade()
            .ok_or("native font measurement context expired")?;
        if !std::sync::Arc::ptr_eq(&system, cx.text_system()) {
            return Err("native font measurements belong to another text system");
        }
        Ok(())
    }
    pub fn capabilities(&self, cx: &App) -> Result<NativeFontCapabilities, &'static str> {
        self.validate_context(cx)?;
        Ok(NativeFontCapabilities {
            bank_id: self.bank_id,
            metrics: self.base_metrics,
            normal: self.fonts[0].is_some(),
            bold: self.fonts[1].is_some(),
            italic: self.fonts[2].is_some(),
            bold_italic: self.fonts[3].is_some(),
        })
    }
    /// Correlate a prepared frame's plain bank/geometry with this live UI bank
    /// before returning the native font. This does not validate input authority,
    /// glyph coverage, shaping or raster ink bounds.
    pub fn get_compatible_font(
        &self,
        bank_id: u64,
        metrics: rune_display::FontMetrics,
        variant: NativeFontVariant,
        cx: &App,
    ) -> Result<&gpui::Font, &'static str> {
        let capabilities = self.capabilities(cx)?;
        if !capabilities.supports_variant(bank_id, metrics, variant) {
            return Err("prepared frame does not match an available measured font variant");
        }
        self.measured_font(variant)
    }
    /// Raw measured face lookup. A future painter must additionally correlate
    /// its prepared frame; prefer get_compatible_font for that combined gate.
    pub fn get_font(
        &self,
        variant: NativeFontVariant,
        cx: &App,
    ) -> Result<&gpui::Font, &'static str> {
        self.validate_context(cx)?;
        self.measured_font(variant)
    }
    fn measured_font(&self, variant: NativeFontVariant) -> Result<&gpui::Font, &'static str> {
        let slot = match variant {
            NativeFontVariant::Normal => 0,
            NativeFontVariant::Bold => 1,
            NativeFontVariant::Italic => 2,
            NativeFontVariant::BoldItalic => 3,
        };
        self.fonts[slot]
            .as_ref()
            .map(|font| &font.font)
            .ok_or(self.errors[slot].unwrap_or("native font style unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_variants_report_explicit_errors_without_fabricating_fonts() {
        let bank = NativeFontVariantBank {
            fonts: [None, None, None, None],
            bank_id: 0,
            base_metrics: rune_display::FixedMetrics::default().0,
            system: std::sync::Weak::new(),
            errors: [
                Some("normal missing"),
                Some("bold missing"),
                Some("italic missing"),
                Some("combined missing"),
            ],
            _ui_owner: std::marker::PhantomData,
        };
        for (variant, error) in [
            (NativeFontVariant::Normal, "normal missing"),
            (NativeFontVariant::Bold, "bold missing"),
            (NativeFontVariant::Italic, "italic missing"),
            (NativeFontVariant::BoldItalic, "combined missing"),
        ] {
            assert_eq!(bank.measured_font(variant).unwrap_err(), error);
        }
    }
}
