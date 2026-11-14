//! Platform-independent guards around font measurements. Standalone-testable
//! without downloading or linking GPUI; does not validate the native font API.
/// Canonical below-baseline distance. GPUI's Linux CosmicText backend returns
/// a signed negative descender; other backends may already return a distance.
/// Reject nonfinite data before turning the signed convention into a magnitude.
pub(crate) fn canonical_descent(raw: f32) -> Result<f32, &'static str> {
    if !raw.is_finite() {
        return Err("invalid native font descent");
    }
    Ok(raw.abs())
}
pub(crate) fn validate(
    size: f32,
    advances: [f32; 3],
    ascent: f32,
    descent: f32,
    line_gap: f32,
) -> Result<(), &'static str> {
    if !size.is_finite()
        || size <= 0.0
        || !advances.iter().all(|n| n.is_finite() && *n > 0.0)
        || ![ascent, descent, line_gap]
            .iter()
            .all(|n| n.is_finite() && *n >= 0.0)
        || ascent == 0.0
        || !(ascent + descent + line_gap).is_finite()
    {
        return Err("invalid native font measurements");
    }
    // Fixed-cell renderer requires equal advances for representative ASCII.
    // This is a guard, not proof of Unicode-wide monospacing or shaping parity.
    let tolerance = (advances[0] * 0.001).max(0.01);
    if advances
        .iter()
        .any(|n| (*n - advances[0]).abs() > tolerance)
    {
        return Err("native first-light requires a monospaced font");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_fractional_monospaced_measurements() {
        assert_eq!(validate(17., [8.125, 8.125, 8.125], 13., 4., 3.), Ok(()));
    }
    #[test]
    fn rejects_proportional_ascii_and_invalid_measurements() {
        assert!(validate(17., [4., 12., 8.], 13., 4., 3.).is_err());
        for bad in [f32::NAN, f32::INFINITY, 0., -1.] {
            assert!(validate(bad, [8.; 3], 13., 4., 3.).is_err());
            assert!(validate(17., [bad; 3], 13., 4., 3.).is_err());
            assert!(validate(17., [8.; 3], bad, 4., 3.).is_err());
        }
        assert!(validate(17., [8.; 3], f32::MAX, f32::MAX, 0.).is_err());
        assert!(validate(17., [8.; 3], 13., -1., 3.).is_err());
    }
}

/// Representative measurements for a real, already-resolved native face.
/// Passing this guard is not proof of Unicode-wide cell/shaping equivalence.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Measurements {
    pub advances: [f32; 3],
    pub ascent: f32,
    pub descent: f32,
}

/// A fixed-grid style variant may change ink, but must not silently change the
/// representative advance or exceed the baseline cell's vertical ink budget.
/// Actual GPUI font resolution and fallback identity still require validation by
/// the UI owner; this pure guard alone cannot establish that a bold face exists.
pub(crate) fn validate_variant(
    size: f32,
    base: Measurements,
    variant: Measurements,
    line_gap: f32,
) -> Result<(), &'static str> {
    validate(size, base.advances, base.ascent, base.descent, line_gap)?;
    validate(
        size,
        variant.advances,
        variant.ascent,
        variant.descent,
        line_gap,
    )?;
    let tolerance = (base.advances[0] * 0.001).max(0.01);
    if variant
        .advances
        .iter()
        .any(|advance| (*advance - base.advances[0]).abs() > tolerance)
    {
        return Err("native font style changes the fixed-cell advance");
    }
    if variant.ascent > base.ascent + tolerance || variant.descent > base.descent + tolerance {
        return Err("native font style exceeds the fixed-cell vertical metrics");
    }
    Ok(())
}
#[cfg(test)]
mod variant_tests {
    use super::*;
    fn base() -> Measurements {
        Measurements {
            advances: [8.125; 3],
            ascent: 13.,
            descent: 4.,
        }
    }
    #[test]
    fn accepts_same_grid_and_smaller_ink_metrics() {
        assert_eq!(validate_variant(17., base(), base(), 3.), Ok(()));
        assert_eq!(
            validate_variant(
                17.,
                base(),
                Measurements {
                    ascent: 12.,
                    descent: 3.,
                    ..base()
                },
                3.
            ),
            Ok(())
        );
    }
    #[test]
    fn rejects_variant_width_or_vertical_budget_changes() {
        assert!(
            validate_variant(
                17.,
                base(),
                Measurements {
                    advances: [9.; 3],
                    ..base()
                },
                3.
            )
            .is_err()
        );
        assert!(
            validate_variant(
                17.,
                base(),
                Measurements {
                    advances: [8.125, 8.125, 9.],
                    ..base()
                },
                3.
            )
            .is_err()
        );
        assert!(
            validate_variant(
                17.,
                base(),
                Measurements {
                    ascent: 14.,
                    ..base()
                },
                3.
            )
            .is_err()
        );
        assert!(
            validate_variant(
                17.,
                base(),
                Measurements {
                    descent: 5.,
                    ..base()
                },
                3.
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_nonfinite_and_invalid_base_or_variant_metrics() {
        for value in [f32::NAN, f32::INFINITY, -1., 0.] {
            assert!(
                validate_variant(
                    17.,
                    base(),
                    Measurements {
                        advances: [value; 3],
                        ..base()
                    },
                    3.
                )
                .is_err()
            );
            assert!(
                validate_variant(
                    17.,
                    Measurements {
                        advances: [value; 3],
                        ..base()
                    },
                    base(),
                    3.
                )
                .is_err()
            );
            assert!(
                validate_variant(
                    17.,
                    base(),
                    Measurements {
                        ascent: value,
                        ..base()
                    },
                    3.
                )
                .is_err()
            );
        }
        assert!(validate_variant(17., base(), base(), f32::NAN).is_err());
    }
}

#[cfg(test)]
mod descent_tests {
    use super::*;
    #[test]
    fn signed_linux_and_positive_backend_descents_share_canonical_geometry() {
        assert_eq!(canonical_descent(-4.0092773), Ok(4.0092773));
        assert_eq!(canonical_descent(4.0092773), Ok(4.0092773));
        let descent = canonical_descent(-4.0092773).unwrap();
        assert_eq!(
            validate(17., [10.234863; 3], 15.779785, descent, 3.),
            Ok(())
        );
        assert_eq!(canonical_descent(-0.), Ok(0.));
    }
    #[test]
    fn nonfinite_descents_never_reach_layout() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(canonical_descent(value).is_err());
        }
    }
}
