//! Platform-independent guards around font measurements. Standalone-testable
//! without downloading or linking GPUI; does not validate the native font API.
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
        || ![ascent, descent, line_gap].iter().all(|n| n.is_finite() && *n >= 0.0)
        || ascent == 0.0
        || !(ascent + descent + line_gap).is_finite()
    {
        return Err("invalid native font measurements");
    }
    // Fixed-cell renderer requires equal advances for representative ASCII.
    // This is a guard, not proof of Unicode-wide monospacing or shaping parity.
    let tolerance = (advances[0] * 0.001).max(0.01);
    if advances.iter().any(|n| (*n - advances[0]).abs() > tolerance) {
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
