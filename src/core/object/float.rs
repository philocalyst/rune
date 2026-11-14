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
    } else if value == 0.0 {
        write!(f, "{value:.1}")
    } else {
        fmt_finite_float(value, f)
    }
}

/// Match GNU's default dtoastr policy, not a user float-output-format override.
/// Normal doubles start at DBL_DIG significant digits; subnormals start at one.
/// Increase precision until decimal parsing preserves the original IEEE bits.
/// https://github.com/emacs-mirror/emacs/blob/emacs-30.1/lib/ftoastr.c
fn fmt_finite_float(value: f64, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    let first = if value.abs() < f64::MIN_POSITIVE { 1 } else { f64::DIGITS as usize };
    for precision in first..=17 {
        let scientific = format!("{value:.decimals$e}", decimals = precision - 1);
        if scientific
            .parse::<f64>()
            .is_ok_and(|parsed| parsed.to_bits() == value.to_bits())
        {
            return fmt_general_float(&scientific, precision, f);
        }
    }
    // Seventeen significant digits round-trip every finite IEEE double. Keep
    // the fallback readable if an unusual target formatter violates that rule.
    write!(f, "{value}")
}

fn fmt_general_float(
    scientific: &str,
    precision: usize,
    f: &mut std::fmt::Formatter,
) -> std::fmt::Result {
    let Some((coefficient, exponent)) = scientific.split_once('e') else {
        return f.write_str(scientific);
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return f.write_str(scientific);
    };
    let sign = if coefficient.starts_with('-') { "-" } else { "" };
    let digits: String = coefficient.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    if digits.is_empty() {
        return f.write_str(scientific);
    }
    // C %g uses scientific notation below 1e-4, or when its rounded decimal
    // exponent reaches the selected precision. Its exponent has a sign and at
    // least two digits. Retain .0 only for fixed integral output (print.c).
    if exponent < -4 || exponent >= precision as i32 {
        write!(f, "{sign}{}", &digits[..1])?;
        if digits.len() > 1 {
            write!(f, ".{}", &digits[1..])?;
        }
        write!(f, "e{exponent:+03}")
    } else if exponent < 0 {
        write!(f, "{sign}0.")?;
        for _ in 0..(-exponent - 1) {
            f.write_str("0")?;
        }
        f.write_str(digits)
    } else {
        let whole = exponent as usize + 1;
        if whole < digits.len() {
            write!(f, "{sign}{}.{}", &digits[..whole], &digits[whole..])
        } else {
            write!(f, "{sign}{digits}")?;
            for _ in digits.len()..whole {
                f.write_str("0")?;
            }
            f.write_str(".0")
        }
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
    fn finite_literals_follow_gnu_default_precision_and_exponent_policy() {
        for (value, expected) in [
            (1e-300, "1e-300"),
            (f64::from_bits(1), "5e-324"),
            (1e-5, "1e-05"),
            (1e-4, "0.0001"),
            (1e14, "100000000000000.0"),
            (1e15, "1e+15"),
            (1e20, "1e+20"),
            (1.0000000000000002, "1.0000000000000002"),
            (1e15 + 0.25, "1000000000000000.2"),
            (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
            (f64::MAX, "1.7976931348623157e+308"),
        ] {
            assert_eq!(LispFloat::new(value, false).to_string(), expected);
            let negative = format!("-{expected}");
            assert_eq!(LispFloat::new(-value, false).to_string(), negative);
        }
    }

    #[test]
    fn finite_bit_patterns_round_trip_with_bounded_output() {
        let mut bits = 0x1234_5678_9abc_def0_u64;
        for _ in 0..4096 {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            let value = f64::from_bits(bits);
            if !value.is_finite() {
                continue;
            }
            let text = LispFloat::new(value, false).to_string();
            assert!(text.len() <= 25, "{text}");
            assert!(text.contains('.') || text.contains('e'), "{text}");
            assert_eq!(text.parse::<f64>().unwrap().to_bits(), bits, "{text}");
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
