use super::{CloneIn, IntoObject};
use crate::core::gc::{Block, GcHeap, GcState, Trace};
use crate::derive_GcMoveable;
use rune_macros::Trace;
use std::fmt::{Debug, Display};

/// A wrapper type for floats to work around issues with Eq. Rust only allows
/// types to be used in match statements if they derive Eq. Even if you never
/// actually use that field in a match. So we need a float wrapper that
/// implements that trait.
#[derive(PartialEq, Trace)]
pub(crate) struct LispFloat(GcHeap<f64>);

derive_GcMoveable!(LispFloat);

impl std::ops::Deref for LispFloat {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl LispFloat {
    pub fn new(float: f64, constant: bool) -> Self {
        LispFloat(GcHeap::new(float, constant))
    }
}

impl Trace for f64 {
    fn trace(&self, _: &mut GcState) {}
}

impl Eq for LispFloat {}

impl<'new> CloneIn<'new, &'new LispFloat> for LispFloat {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> super::Gc<&'new Self> {
        (**self).into_obj(bk)
    }
}

impl Display for LispFloat {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt_float(**self, f)
    }
}

/// Format a numeric literal, not Rust's `inf`/`NaN` symbol spellings. Share
/// this boundary with generated Lisp fixtures so nonfinite cases stay numbers.
pub(crate) fn fmt_float(value: f64, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    if value.is_nan() {
        if value.is_sign_negative() {
            f.write_str("-0.0e+NaN")
        } else {
            f.write_str("0.0e+NaN")
        }
    } else if value.is_infinite() {
        if value.is_sign_negative() {
            f.write_str("-1.0e+INF")
        } else {
            f.write_str("1.0e+INF")
        }
    } else if value.fract() == 0.0 {
        write!(f, "{value:.1}")
    } else {
        write!(f, "{value}")
    }
}

impl Debug for LispFloat {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_literals_preserve_ieee_classes_and_signed_zero() {
        for (value, expected) in [
            (f64::INFINITY, "1.0e+INF"),
            (f64::NEG_INFINITY, "-1.0e+INF"),
            (f64::NAN, "0.0e+NaN"),
            (-f64::NAN, "-0.0e+NaN"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (-1.5, "-1.5"),
        ] {
            assert_eq!(LispFloat::new(value, false).to_string(), expected);
        }
    }

    #[test]
    fn rendered_numbers_read_back_as_float_classes_not_symbols() {
        use crate::core::gc::{Context, RootSet};
        use crate::core::object::Number;
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for value in [
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            -f64::NAN,
            0.0,
            -0.0,
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            f64::MAX,
            -1.5,
        ] {
            let literal = LispFloat::new(value, false).to_string();
            let object = crate::reader::read(&literal, &cx).unwrap().0;
            let actual = Number::try_from(object).unwrap().to_float();
            assert!(object.is_float(), "{literal}");
            if value.is_nan() {
                assert!(actual.is_nan());
                assert_eq!(actual.is_sign_negative(), value.is_sign_negative());
            } else {
                assert_eq!(actual.to_bits(), value.to_bits(), "{literal}");
            }
        }
    }
}
