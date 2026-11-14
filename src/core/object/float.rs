use super::{CloneIn, IntoObject};
use crate::core::gc::{Block, GcHeap, GcState, Trace};
use crate::derive_GcMoveable;
use rune_macros::Trace;
use std::fmt::{Debug, Display};

/// A wrapper type for floats to work around issues with Eq. Rust only allows
/// types to be used in match statements if they derive Eq. Even if you never
/// actually use that field in a match. So we need a float wrapper that
/// implements that trait.
#[derive(Trace)]
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

// Lisp `equal`/`eql` compare float representations, unlike numeric `=`.
// This also makes Eq reflexive for NaNs and distinguishes both signed zeros.
impl PartialEq for LispFloat {
    fn eq(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl Eq for LispFloat {}

impl<'new> CloneIn<'new, &'new LispFloat> for LispFloat {
    fn clone_in<const C: bool>(&self, bk: &'new Block<C>) -> anyhow::Result<super::Gc<&'new Self>> {
        Ok((**self).into_obj(bk))
    }
}

impl Display for LispFloat {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt_float(**self, f)
    }
}

const NAN_PAYLOAD_MASK: u64 = (1_u64 << 51) - 1;

/// GNU lread.c stores the leading decimal integer modulo 2^51 in a quiet
/// NaN's payload. Fractional digits do not contribute. A dot-leading number
/// starts with digit_to_number('.') == -2, retained as an unsigned sentinel.
/// Callers have already validated numeric syntax and stripped a leading sign.
pub(crate) fn nan_from_decimal_prefix(prefix: &str) -> f64 {
    let payload = if prefix.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        prefix.bytes().take_while(u8::is_ascii_digit).fold(0_u64, |value, digit| {
            value.wrapping_mul(10).wrapping_add(u64::from(digit - b'0'))
        })
    } else {
        u64::MAX - 1
    };
    f64::from_bits(0x7ff8_0000_0000_0000 | (payload & NAN_PAYLOAD_MASK))
}

/// Format a numeric literal, not Rust's `inf`/`NaN` symbol spellings. Share
/// this boundary with generated Lisp fixtures so nonfinite cases stay numbers.
pub(crate) fn fmt_float(value: f64, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    if value.is_nan() {
        let payload = value.to_bits() & NAN_PAYLOAD_MASK;
        if value.is_sign_negative() {
            write!(f, "-{payload}.0e+NaN")
        } else {
            write!(f, "{payload}.0e+NaN")
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

#[derive(Clone, Copy)]
struct FloatOutputSpec {
    kind: u8,
    precision: usize,
    explicit_precision: bool,
}

fn float_output_spec(format: &str) -> Option<FloatOutputSpec> {
    // GNU's format validator reads the string as a C string.
    let format = format.split('\0').next()?;
    let suffix = format.strip_prefix("%.")?;
    let (&kind, digits) = suffix.as_bytes().split_last()?;
    if !matches!(kind, b'e' | b'f' | b'g') {
        return None;
    }
    let mut precision = 0;
    for &digit in digits {
        if !digit.is_ascii_digit() {
            return None;
        }
        precision = precision * 10 + usize::from(digit - b'0');
        if precision > f64::DIGITS as usize {
            return None;
        }
    }
    let explicit_precision = !digits.is_empty();
    if explicit_precision && precision == 0 && kind != b'f' {
        return None;
    }
    Some(FloatOutputSpec { kind, precision, explicit_precision })
}

/// Scoped policy boundary for number-to-string. No user string is passed as a
/// Rust/C format program: only validated kind and precision select operations.
pub(crate) fn float_to_string_with_format(value: f64, format: Option<&str>) -> String {
    struct Output(f64, Option<FloatOutputSpec>);
    impl Display for Output {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            let Self(value, spec) = self;
            let Some(spec) = spec.filter(|_| value.is_finite()) else {
                return fmt_float(*value, f);
            };
            let precision = spec.precision;
            match spec.kind {
                b'f' => {
                    write!(f, "{value:.precision$}")?;
                    // %.0f deliberately omits the decimal point; %.f does not.
                    if precision == 0 && !spec.explicit_precision {
                        f.write_str(".0")?;
                    }
                    Ok(())
                }
                b'e' => {
                    let scientific = format!("{value:.precision$e}");
                    if let Some((coefficient, exponent)) = scientific.split_once('e') {
                        if let Ok(exponent) = exponent.parse::<i32>() {
                            return write!(f, "{coefficient}e{exponent:+03}");
                        }
                    }
                    f.write_str(&scientific)
                }
                b'g' => {
                    let precision = precision.max(1);
                    let scientific = format!("{value:.decimals$e}", decimals = precision - 1);
                    fmt_general_float(&scientific, precision, f)
                }
                _ => fmt_float(*value, f),
            }
        }
    }
    Output(value, format.and_then(float_output_spec)).to_string()
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
        return write!(f, "{sign}0.0");
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
    fn finite_output_policy_matches_gnu_precision_notation_and_fallback_rules() {
        for (policy, value, expected) in [
            ("%.2f", 1.23456, "1.23"),
            ("%.0f", 2.5, "2"),
            ("%.0f", -2.5, "-2"),
            ("%.f", 2.5, "2.0"),
            ("%.2e", 1234.0, "1.23e+03"),
            ("%.e", 1.23456, "1e+00"),
            ("%.3g", 1234.0, "1.23e+03"),
            ("%.3g", 1.0, "1.0"),
            ("%.g", -0.0, "-0.0"),
            ("%.0g", 1.23456, "1.23456"),
            ("%.16f", 1.23456, "1.23456"),
            ("%f", 1.23456, "1.23456"),
            ("%.03g", 0.000012345, "1.23e-05"),
            ("%.2f\0ignored", 1.23456, "1.23"),
            ("%.2f", f64::INFINITY, "1.0e+INF"),
            ("%.2f", nan_from_decimal_prefix("1"), "1.0e+NaN"),
            ("%.😃f", 1.23456, "1.23456"),
        ] {
            assert_eq!(float_to_string_with_format(value, Some(policy)), expected);
        }
    }

    #[test]
    fn structural_float_equality_is_bitwise_and_hash_consistent() {
        use std::collections::HashSet;
        let roots = crate::core::gc::RootSet::default();
        let cx = crate::core::gc::Context::new(&roots);
        for bits in [
            0_u64,
            1_u64 << 63,
            0x7ff8_0000_0000_0000,
            0x7ff8_0000_0000_0001,
            0xfff8_0000_0000_0001,
        ] {
            let left = cx.add(f64::from_bits(bits));
            let right = cx.add(f64::from_bits(bits));
            assert!(!left.ptr_eq(right));
            assert_eq!(left, right);
            let keys = HashSet::from([left]);
            assert!(keys.contains(&right));
        }
        assert_ne!(cx.add(0.0), cx.add(-0.0));
        assert_ne!(
            cx.add(f64::from_bits(0x7ff8_0000_0000_0000)),
            cx.add(f64::from_bits(0x7ff8_0000_0000_0001))
        );
    }

    #[test]
    fn lisp_structural_float_comparison_is_distinct_from_numeric_ieee_comparison() {
        for (source, expected) in [
            ("(equal 0.0e+NaN 0.0e+NaN)", "t"),
            ("(equal 1.0e+NaN 1.0e+NaN)", "t"),
            ("(equal 0.0e+NaN 1.0e+NaN)", "nil"),
            ("(equal 1.0e+NaN -1.0e+NaN)", "nil"),
            ("(equal 0.0 -0.0)", "nil"),
            ("(equal '(1.0e+NaN) '(1.0e+NaN))", "t"),
            ("(equal [1.0e+NaN] [1.0e+NaN])", "t"),
            ("(equal '(0.0) '(-0.0))", "nil"),
            ("(equal [0.0] [-0.0])", "nil"),
            ("(= 0.0 -0.0)", "t"),
            ("(= 1.0e+NaN 1.0e+NaN)", "nil"),
            ("(< 1.0e+NaN 2.0)", "nil"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn quiet_nan_payload_literals_preserve_all_51_payload_bits() {
        let mut payload = 1_u64;
        for _ in 0..4096 {
            payload ^= payload << 13;
            payload ^= payload >> 7;
            payload ^= payload << 17;
            payload &= NAN_PAYLOAD_MASK;
            for negative in [false, true] {
                let bits = 0x7ff8_0000_0000_0000 | payload | (u64::from(negative) << 63);
                let value = LispFloat::new(f64::from_bits(bits), false);
                let literal = value.to_string();
                let roots = crate::core::gc::RootSet::default();
                let cx = crate::core::gc::Context::new(&roots);
                let parsed = crate::reader::read(&literal, &cx).unwrap().0;
                let crate::core::object::ObjectType::Float(parsed) = parsed.untag() else {
                    panic!("float expected")
                };
                assert_eq!(parsed.to_bits(), bits);
            }
        }
    }

    #[test]
    fn signaling_nan_literal_readback_is_explicitly_quieted() {
        let roots = crate::core::gc::RootSet::default();
        let cx = crate::core::gc::Context::new(&roots);
        for sign in [0, 1_u64 << 63] {
            let bits = sign | 0x7ff0_0000_0000_0001;
            let literal = LispFloat::new(f64::from_bits(bits), false).to_string();
            let crate::core::object::ObjectType::Float(parsed) =
                crate::reader::read(&literal, &cx).unwrap().0.untag()
            else {
                panic!("float expected")
            };
            assert_eq!(parsed.to_bits(), bits | (1_u64 << 51));
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
