//! UI-only smooth-wheel quantization. Identity changes discard fractional input.
#[derive(Default)]
pub(crate) struct WheelLines {
    scope: Option<(u64, u64, u64)>,
    remainder: f32,
}
impl WheelLines {
    pub(crate) fn take(&mut self, scope: (u64, u64, u64), dy: f32, line_height: f32) -> i32 {
        if !dy.is_finite() || !line_height.is_finite() || line_height <= 0. {
            return 0;
        }
        if self.scope != Some(scope) {
            self.scope = Some(scope);
            self.remainder = 0.;
        }
        // GPUI positive delta is toward document beginning. Bound a single
        // callback, not the document; excess is intentionally not queued.
        let total = self.remainder - dy / line_height;
        let lines = total.trunc().clamp(-256., 256.) as i32;
        self.remainder = if total.abs() > 256. { 0. } else { total - lines as f32 };
        lines
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_wheel_is_bounded_and_never_leaks_between_scopes() {
        let mut w = WheelLines::default();
        let a = (1, 2, 3);
        assert_eq!(w.take(a, -5., 10.), 0);
        assert_eq!(w.take(a, -5., 10.), 1);
        assert_eq!(w.take(a, 10., 10.), -1);
        assert_eq!(w.take(a, -9., 10.), 0);
        assert_eq!(w.take((1, 4, 3), -2., 10.), 0);
        assert_eq!(w.take(a, f32::NAN, 10.), 0);
        assert_eq!(w.take(a, -f32::MAX, 10.), 256);
        assert_eq!(w.take(a, -1., 10.), 0);
        assert_eq!(w.take(a, 1., 0.), 0);
    }
}
