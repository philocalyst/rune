//! Plain measured-font correlation data. No GPUI or Lisp values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasuredFontVariant {
    Normal,
    Bold,
    Italic,
    BoldItalic,
}
/// Plain correlation/measurement data for a future owner/worker handshake.
/// The ID is not input authority or a security credential; it prevents mixing
/// frames prepared for different measured banks. No font object/ID is exposed.
/// Availability means measured face compatibility, NOT enabled FrameView styling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasuredFontCapabilities {
    pub bank_id: u64,
    pub metrics: crate::FontMetrics,
    pub normal: bool,
    pub bold: bool,
    pub italic: bool,
    pub bold_italic: bool,
}
impl MeasuredFontCapabilities {
    /// Correlation and finite baseline checks only; not input/security authority.
    pub fn matches_geometry(self, bank_id: u64, metrics: crate::FontMetrics) -> bool {
        self.bank_id != 0
            && self.bank_id == bank_id
            && self.normal
            && self.metrics.valid()
            && metrics.valid()
            && self.metrics == metrics
    }
    /// Combined prerequisite check for consumers: never accept an availability
    /// flag from a stale/different bank or incompatible baseline geometry.
    /// This still does not authorize input or establish actual glyph painting.
    pub fn supports_variant(
        self,
        bank_id: u64,
        metrics: crate::FontMetrics,
        variant: MeasuredFontVariant,
    ) -> bool {
        self.matches_geometry(bank_id, metrics) && self.has_compatible_face(variant)
    }
    pub fn has_compatible_face(self, variant: MeasuredFontVariant) -> bool {
        match variant {
            MeasuredFontVariant::Normal => self.normal,
            MeasuredFontVariant::Bold => self.bold,
            MeasuredFontVariant::Italic => self.italic,
            MeasuredFontVariant::BoldItalic => self.bold_italic,
        }
    }
}
#[cfg(test)]
mod capability_tests {
    use super::*;
    #[test]
    fn plain_availability_is_send_sync_and_does_not_invent_missing_faces() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MeasuredFontCapabilities>();
        let capabilities = MeasuredFontCapabilities {
            bank_id: 7,
            metrics: crate::FixedMetrics::default().0,
            normal: true,
            bold: true,
            italic: false,
            bold_italic: false,
        };
        assert!(capabilities.has_compatible_face(MeasuredFontVariant::Normal));
        assert!(capabilities.has_compatible_face(MeasuredFontVariant::Bold));
        assert!(!capabilities.has_compatible_face(MeasuredFontVariant::Italic));
        assert!(!capabilities.has_compatible_face(MeasuredFontVariant::BoldItalic));
        assert_eq!(
            std::thread::spawn(move || capabilities).join().unwrap(),
            capabilities
        );
    }
    #[test]
    fn stale_or_invalid_geometry_is_not_accepted() {
        let metrics = crate::FixedMetrics::default().0;
        let capabilities = MeasuredFontCapabilities {
            bank_id: 7,
            metrics,
            normal: true,
            bold: false,
            italic: false,
            bold_italic: false,
        };
        assert!(capabilities.matches_geometry(7, metrics));
        assert!(!capabilities.matches_geometry(8, metrics));
        let mut wrong = metrics;
        wrong.advance += 1.;
        assert!(!capabilities.matches_geometry(7, wrong));
        wrong.advance = f32::NAN;
        assert!(!capabilities.matches_geometry(7, wrong));
        assert!(
            !MeasuredFontCapabilities {
                bank_id: 0,
                ..capabilities
            }
            .matches_geometry(0, metrics)
        );
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn availability_cannot_bypass_bank_baseline_or_normal_face() {
        let m = crate::FixedMetrics::default().0;
        let c = MeasuredFontCapabilities {
            bank_id: 9,
            metrics: m,
            normal: true,
            bold: true,
            italic: false,
            bold_italic: false,
        };
        assert!(c.supports_variant(9, m, MeasuredFontVariant::Bold));
        assert!(!c.supports_variant(8, m, MeasuredFontVariant::Bold));
        assert!(!c.supports_variant(9, m, MeasuredFontVariant::Italic));
        assert!(
            !MeasuredFontCapabilities { normal: false, ..c }.supports_variant(
                9,
                m,
                MeasuredFontVariant::Bold
            )
        );
        for i in 0..4 {
            let mut wrong = m;
            match i {
                0 => wrong.advance += 1.,
                1 => wrong.ascent += 1.,
                2 => wrong.descent += 1.,
                _ => wrong.line_gap += 1.,
            }
            assert!(!c.supports_variant(9, wrong, MeasuredFontVariant::Bold));
        }
    }
    #[test]
    fn nonfinite_values_never_admit_even_when_equal_availability_is_true() {
        let m = crate::FixedMetrics::default().0;
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for i in 0..4 {
                let mut wrong = m;
                match i {
                    0 => wrong.advance = v,
                    1 => wrong.ascent = v,
                    2 => wrong.descent = v,
                    _ => wrong.line_gap = v,
                }
                let c = MeasuredFontCapabilities {
                    bank_id: 9,
                    metrics: wrong,
                    normal: true,
                    bold: true,
                    italic: true,
                    bold_italic: true,
                };
                assert!(!c.supports_variant(9, wrong, MeasuredFontVariant::BoldItalic));
            }
        }
    }
}
