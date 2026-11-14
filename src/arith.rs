//! Lisp arithmetic adapters over the owned numeric model.
pub(crate) use crate::core::object::{MAX_FIXNUM, MIN_FIXNUM, NumberValue};
use crate::core::object::{Number, Object};
use anyhow::Result;
#[cfg(test)]
use num_bigint::BigInt;
use rune_macros::defun;

#[defun(name = "+")]
pub(crate) fn add(vars: &[Number]) -> NumberValue {
    vars.iter().fold(NumberValue::Int(0), |acc, x| acc + x.val())
}

#[defun(name = "-")]
pub(crate) fn sub(number: Option<Number>, numbers: &[Number]) -> NumberValue {
    match number {
        Some(num) => {
            let num = num.val();
            if numbers.is_empty() {
                -num
            } else {
                numbers.iter().fold(num, |acc, x| acc - x.val())
            }
        }
        None => NumberValue::Int(0),
    }
}

#[defun(name = "*")]
pub(crate) fn mul(numbers: &[Number]) -> NumberValue {
    numbers.iter().fold(NumberValue::Int(1), |acc, x| acc * x.val())
}

#[defun(name = "/")]
pub(crate) fn div(number: Number, divisors: &[Number]) -> Result<NumberValue> {
    // Preserve the source-verified whole-expression floating selection. The
    // kernel handles IEEE floats and fallible exact integer operations.
    let floating = number.is_float() || divisors.iter().any(Number::is_float);
    let denominators = if divisors.is_empty() { std::slice::from_ref(&number) } else { divisors };
    let initial = if divisors.is_empty() { NumberValue::Int(1) } else { number.val() };
    let initial = if floating { NumberValue::Float(initial.to_float()) } else { initial };
    Ok(denominators.iter().try_fold(initial, |value, denominator| {
        let denominator = denominator.val();
        let denominator =
            if floating { NumberValue::Float(denominator.to_float()) } else { denominator };
        value.checked_div(denominator)
    })?)
}

#[defun(name = "1+")]
pub(crate) fn add_one(number: Number) -> NumberValue {
    number.val() + NumberValue::Int(1)
}

#[defun(name = "1-")]
pub(crate) fn sub_one(number: Number) -> NumberValue {
    number.val() - NumberValue::Int(1)
}

pub(crate) fn num_eq(number: Number, numbers: &[Number]) -> bool {
    numbers
        .iter()
        .all(|next| number.numeric_cmp(*next) == Some(std::cmp::Ordering::Equal))
}

#[defun(name = "=")]
fn num_eq_lisp(number: Number, numbers: &[Object]) -> Result<bool> {
    for &next in numbers {
        let next = Number::try_from(next)?;
        if number.numeric_cmp(next) != Some(std::cmp::Ordering::Equal) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[defun(name = "/=")]
pub(crate) fn num_ne(number: Number, other: Number) -> bool {
    number.numeric_cmp(other) != Some(std::cmp::Ordering::Equal)
}

fn cmp(number: Number, numbers: &[Number], predicate: fn(std::cmp::Ordering) -> bool) -> bool {
    numbers
        .iter()
        .try_fold(number, |previous, &next| {
            previous.numeric_cmp(next).is_some_and(predicate).then_some(next)
        })
        .is_some()
}

pub(crate) fn less_than(number: Number, numbers: &[Number]) -> bool {
    cmp(number, numbers, |order| order == std::cmp::Ordering::Less)
}

pub(crate) fn less_than_or_eq(number: Number, numbers: &[Number]) -> bool {
    cmp(number, numbers, |order| order != std::cmp::Ordering::Greater)
}

pub(crate) fn greater_than(number: Number, numbers: &[Number]) -> bool {
    cmp(number, numbers, |order| order == std::cmp::Ordering::Greater)
}

pub(crate) fn greater_than_or_eq(number: Number, numbers: &[Number]) -> bool {
    cmp(number, numbers, |order| order != std::cmp::Ordering::Less)
}

// Lisp evaluates all arguments before calling these adapters, but GNU checks
// numeric types only until a comparison fails. Preserve VM helpers' typed APIs
// without eagerly converting an entire variadic slice at the Lisp boundary.
fn cmp_objects(
    number: Number,
    numbers: &[Object],
    predicate: fn(std::cmp::Ordering) -> bool,
) -> Result<bool> {
    let mut previous = number;
    for &next in numbers {
        let next = Number::try_from(next)?;
        if !previous.numeric_cmp(next).is_some_and(predicate) {
            return Ok(false);
        }
        previous = next;
    }
    Ok(true)
}

#[defun(name = "<")]
fn less_than_lisp(number: Number, numbers: &[Object]) -> Result<bool> {
    cmp_objects(number, numbers, |order| order == std::cmp::Ordering::Less)
}

#[defun(name = "<=")]
fn less_than_or_eq_lisp(number: Number, numbers: &[Object]) -> Result<bool> {
    cmp_objects(number, numbers, |order| order != std::cmp::Ordering::Greater)
}

#[defun(name = ">")]
fn greater_than_lisp(number: Number, numbers: &[Object]) -> Result<bool> {
    cmp_objects(number, numbers, |order| order == std::cmp::Ordering::Greater)
}

#[defun(name = ">=")]
fn greater_than_or_eq_lisp(number: Number, numbers: &[Object]) -> Result<bool> {
    cmp_objects(number, numbers, |order| order != std::cmp::Ordering::Less)
}

#[defun]
pub(crate) fn logior(ints_or_markers: &[Number]) -> Result<NumberValue> {
    Ok(ints_or_markers
        .iter()
        .try_fold(NumberValue::Int(0), |value, next| value.bit_or(next.val()))?)
}

#[defun]
fn logand(int_or_markers: &[Number]) -> Result<NumberValue> {
    Ok(int_or_markers
        .iter()
        .try_fold(NumberValue::Int(-1), |value, next| value.bit_and(next.val()))?)
}

#[defun]
fn logxor(integers: &[Number]) -> Result<NumberValue> {
    Ok(integers
        .iter()
        .try_fold(NumberValue::Int(0), |value, next| value.bit_xor(next.val()))?)
}

#[defun]
fn lognot(integer: Number) -> Result<NumberValue> {
    Ok(integer.val().bit_not()?)
}

#[defun]
fn logcount(integer: Number) -> Result<NumberValue> {
    Ok(integer.val().bit_count()?)
}

#[defun]
fn fixnump(value: crate::core::object::Object) -> bool {
    value.is_int()
}

#[defun]
fn bignump(value: crate::core::object::Object) -> bool {
    value.is_big_int()
}

fn remainder_driver(x: Number, y: Number, modulo: bool) -> Result<NumberValue> {
    Ok(x.val().checked_rem(y.val(), modulo)?)
}

#[defun(name = "mod")]
pub(crate) fn modulo(x: Number, y: Number) -> Result<NumberValue> {
    remainder_driver(x, y, true)
}

#[defun(name = "%")]
pub(crate) fn remainder(x: Number, y: Number) -> Result<NumberValue> {
    // TODO: Handle markers
    remainder_driver(x, y, false)
}

#[expect(clippy::trivially_copy_pass_by_ref)]
fn max_val(x: NumberValue, y: &Number) -> NumberValue {
    x.maximum(y.val())
}

#[expect(clippy::trivially_copy_pass_by_ref)]
fn min_val(x: NumberValue, y: &Number) -> NumberValue {
    x.minimum(y.val())
}

#[defun]
pub(crate) fn max(number_or_marker: Number, number_or_markers: &[Number]) -> NumberValue {
    number_or_markers.iter().fold(number_or_marker.val(), max_val)
}

#[defun]
pub(crate) fn min(number_or_marker: Number, number_or_markers: &[Number]) -> NumberValue {
    number_or_markers.iter().fold(number_or_marker.val(), min_val)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::core::{
        gc::{Context, RootSet},
        object::IntoObject,
    };

    #[test]
    fn test_add() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_eq!(add(&[]), NumberValue::Int(0));
        assert_eq!(add(&[7.into(), 13.into()]), NumberValue::Int(20));
        assert_eq!(add(&[1.into(), cx.add_as(2.5)]), NumberValue::Float(3.5));
        assert_eq!(add(&[0.into(), (-1).into()]), NumberValue::Int(-1));
    }

    #[test]
    fn m0_integer_arithmetic_promotes_instead_of_clamping_or_overflowing() {
        assert_eq!(
            NumberValue::Int(MAX_FIXNUM) + NumberValue::Int(1),
            NumberValue::Big(BigInt::from(MAX_FIXNUM) + 1_u8)
        );
        assert_eq!(
            NumberValue::Int(MIN_FIXNUM) - NumberValue::Int(1),
            NumberValue::Big(BigInt::from(MIN_FIXNUM) - 1_u8)
        );
        assert_eq!(-NumberValue::Int(MIN_FIXNUM), NumberValue::Big(-BigInt::from(MIN_FIXNUM)));
        assert_eq!(
            NumberValue::Int(i64::MAX) + NumberValue::Int(1),
            NumberValue::Big(BigInt::from(i64::MAX) + 1_u8)
        );
        assert_eq!(
            NumberValue::Int(i64::MIN) - NumberValue::Int(1),
            NumberValue::Big(BigInt::from(i64::MIN) - 1_u8)
        );
        assert_eq!(-NumberValue::Int(i64::MIN), NumberValue::Big(-BigInt::from(i64::MIN)));
        assert_eq!(
            NumberValue::Int(i64::MAX) * NumberValue::Int(i64::MAX),
            NumberValue::Big(BigInt::from(i64::MAX).pow(2))
        );
        assert_eq!(
            NumberValue::Big(BigInt::from(MAX_FIXNUM) + 1_u8) - NumberValue::Int(1),
            NumberValue::Int(MAX_FIXNUM)
        );
        for (source, expected) in [
            ("(+ 36028797018963967 1)", "36028797018963968"),
            ("(- -36028797018963968 1)", "-36028797018963969"),
            ("(- -36028797018963968)", "36028797018963968"),
            ("(* 36028797018963967 2)", "72057594037927934"),
            ("(1+ 36028797018963967)", "36028797018963968"),
            ("(1- -36028797018963968)", "-36028797018963969"),
            ("(eq (- 36028797018963968 1) 36028797018963967)", "t"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn m0_float_to_integer_checks_exact_tagged_bounds() {
        let first_positive_bignum = -(MIN_FIXNUM as f64);
        assert_eq!(
            NumberValue::Float(first_positive_bignum).coerce_integer(),
            NumberValue::Big(-BigInt::from(MIN_FIXNUM))
        );
        assert_eq!(
            NumberValue::Float(MIN_FIXNUM as f64).coerce_integer(),
            NumberValue::Int(MIN_FIXNUM)
        );
        assert_eq!(
            NumberValue::Float(first_positive_bignum - 4.0).coerce_integer(),
            NumberValue::Int(-MIN_FIXNUM - 4)
        );
        assert_eq!(
            NumberValue::Float(MIN_FIXNUM as f64 - 8.0).coerce_integer(),
            NumberValue::Big(BigInt::from(MIN_FIXNUM) - 8_u8)
        );
        assert_eq!(NumberValue::Float(-1.9).coerce_integer(), NumberValue::Int(-1));
        for (source, expected) in [
            ("(truncate 36028797018963968.0)", "36028797018963968"),
            ("(floor 36028797018963968.0)", "36028797018963968"),
            ("(ceiling 36028797018963968.0)", "36028797018963968"),
            ("(round 36028797018963968.0)", "36028797018963968"),
            ("(truncate -36028797018963976.0)", "-36028797018963976"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn test_sub() {
        assert_eq!(sub(None, &[]), NumberValue::Int(0));
        assert_eq!(sub(Some(7.into()), &[]), NumberValue::Int(-7));
        assert_eq!(sub(Some(7.into()), &[13.into()]), NumberValue::Int(-6));
        assert_eq!(sub(Some(0.into()), &[(-1).into()]), NumberValue::Int(1));
    }

    #[test]
    fn test_mul() {
        assert_eq!(mul(&[]), NumberValue::Int(1));
        assert_eq!(mul(&[7.into(), 13.into()]), NumberValue::Int(91));
        assert_eq!(mul(&[(-1).into(), 1.into()]), NumberValue::Int(-1));
    }

    #[test]
    fn test_div() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);

        assert_eq!(div(cx.add_as(12.0), &[]).unwrap(), NumberValue::Float(1.0 / 12.0));
        assert_eq!(div(12.into(), &[5.into(), 2.into()]).unwrap(), NumberValue::Int(1));
    }

    #[test]
    fn m0_division_rejects_integer_zero_and_preserves_float_rules() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let zero = cx.add_as(BigInt::from(0));
        for dividend in [Number::from(-1073741824_i64), cx.add_as(BigInt::from(i64::MAX) + 1_u8)] {
            for denominator in [Number::from(0_i64), zero] {
                assert!(div(dividend, &[denominator]).is_err());
            }
        }
        assert!(div(0_i64.into(), &[]).is_err());
        assert_eq!(div(2_i64.into(), &[]).unwrap(), NumberValue::Int(0));
        assert_eq!(div((-1_i64).into(), &[]).unwrap(), NumberValue::Int(-1));
        assert_eq!(
            div(MIN_FIXNUM.into(), &[(-1_i64).into()]).unwrap(),
            NumberValue::Big(-BigInt::from(MIN_FIXNUM))
        );
        assert_eq!(
            div(10_i64.into(), &[3_i64.into(), cx.add_as(2.0)]).unwrap(),
            NumberValue::Float(10.0 / 3.0 / 2.0)
        );
        let NumberValue::Float(infinity) =
            div(2_i64.into(), &[0_i64.into(), cx.add_as(2.0)]).unwrap()
        else {
            panic!("any float operand must select floating division");
        };
        assert!(infinity.is_infinite());
        let NumberValue::Float(nan) = div(cx.add_as(0.0), &[0_i64.into()]).unwrap() else {
            panic!("float zero division must remain floating");
        };
        assert!(nan.is_nan());
        drop(cx);
        crate::interpreter::assert_lisp(
            "(condition-case nil (/ -1073741824 0) (error 'caught))",
            "caught",
        );
        crate::interpreter::assert_lisp("(condition-case nil (/ 0) (error 'caught))", "caught");
        crate::interpreter::assert_lisp("(/ 18446744073709551616 2)", "9223372036854775808");
        crate::interpreter::assert_lisp("(/ 2.0)", "0.5");
    }

    #[test]
    fn m0_remainder_and_modulo_distinguish_sign_and_zero_rules() {
        for (source, expected) in [
            ("(% -5 3)", "-2"),
            ("(mod -5 3)", "1"),
            ("(% 5 -3)", "2"),
            ("(mod 5 -3)", "-1"),
            ("(mod -5 -3)", "-2"),
            ("(mod -6 3)", "0"),
            ("(mod -5.5 3)", "0.5"),
            ("(mod 5 -3.0)", "-1.0"),
            ("(% 18446744073709551617 3)", "2"),
            ("(mod -18446744073709551617 3)", "1"),
            ("(% -36028797018963968 -1)", "0"),
            ("(condition-case nil (% 5 0) (error 'caught))", "caught"),
            ("(condition-case nil (mod 5 0) (error 'caught))", "caught"),
            ("(condition-case nil (% 5.0 2) (error 'caught))", "caught"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn m0_remainder_zero_bignums_error_and_float_mod_zero_is_nan() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let zero = cx.add_as(BigInt::from(0));
        for operation in [remainder, modulo] {
            for numerator in [Number::from(5_i64), cx.add_as(BigInt::from(i64::MAX) + 1_u8)] {
                assert!(operation(numerator, zero).is_err());
            }
        }
        for (numerator, denominator) in
            [(cx.add_as(5.0), Number::from(0_i64)), (5_i64.into(), cx.add_as(-0.0))]
        {
            let NumberValue::Float(value) = modulo(numerator, denominator).unwrap() else {
                panic!("floating modulo must return a float");
            };
            assert!(value.is_nan());
        }
    }

    #[test]
    fn test_eq() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        let int1 = 1.into();
        let float1 = cx.add_as(1.0);
        let float1_1 = cx.add_as(1.1);

        assert!(num_eq(int1, &[]));
        assert!(num_eq(int1, &[cx.add_as(1.0)]));
        assert!(num_eq(float1, &[1.into()]));
        assert!(!num_eq(float1, &[1.into(), 1.into(), float1_1]));
    }

    #[test]
    fn comparison_type_checks_short_circuit_without_skipping_argument_effects() {
        for (source, expected) in [
            ("(= 0.0e+NaN 1 'invalid)", "nil"),
            ("(= 1 2 'invalid)", "nil"),
            ("(< 2 1 'invalid)", "nil"),
            ("(<= 2 1 'invalid)", "nil"),
            ("(> 1 2 'invalid)", "nil"),
            ("(>= 1 2 'invalid)", "nil"),
            ("(< 1 0.0e+NaN 'invalid)", "nil"),
            ("(condition-case nil (= 1 1 'invalid) (error 'caught))", "caught"),
            ("(condition-case nil (< 1 2 'invalid) (error 'caught))", "caught"),
            ("(condition-case nil (< 'invalid 2 1) (error 'caught))", "caught"),
            ("(let ((n 0)) (list (< 2 1 (progn (setq n 1) 'invalid)) n))", "(nil 1)"),
            ("(let ((n 0)) (list (= 1 2 (progn (setq n 1) 'invalid)) n))", "(nil 1)"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn test_cmp() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert!(less_than(1.into(), &[]));
        assert!(less_than(1.into(), &[cx.add_as(1.1)]));
        assert!(!less_than(cx.add_as(1.0), &[1.into()]));
        assert!(less_than(cx.add_as(1.0), &[cx.add_as(1.1), 2.into(), cx.add_as(2.1)]));
    }

    #[test]
    fn m0_comparison_chains_compare_adjacent_operands() {
        for (source, expected) in [
            ("(< 1 2 1)", "nil"),
            ("(< -3 -2 -1)", "t"),
            ("(> 3 2 1)", "t"),
            ("(> -1 -2 -1)", "nil"),
            ("(<= 1 2 1)", "nil"),
            ("(<= -3 -2 -2)", "t"),
            ("(>= 3 2 1)", "t"),
            ("(>= -1 -2 -1)", "nil"),
            ("(< 1 2 2)", "nil"),
            ("(> 3 2 2)", "nil"),
            ("(<= 2 2 2)", "t"),
            ("(>= -2 -2 -2)", "t"),
            ("(< 1 2.0 1)", "nil"),
            ("(> 3.0 2 1.0)", "t"),
            ("(< 1 9223372036854775808 2)", "nil"),
            ("(> 9223372036854775809 9223372036854775808 2)", "t"),
            ("(< 1 0.0e+NaN 3)", "nil"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn bit_count_accepts_bignums_and_counts_negative_zero_bits() {
        for (source, expected) in [
            ("(logcount 0)", "0"),
            ("(logcount -1)", "0"),
            ("(logcount -2)", "1"),
            ("(logcount 255)", "8"),
            ("(logcount -256)", "8"),
            ("(logcount 18446744073709551615)", "64"),
            ("(logcount -18446744073709551617)", "1"),
            ("(logcount -9223372036854775808)", "63"),
            ("(condition-case nil (logcount 1.0) (error 'caught))", "caught"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn integer_width_uses_signed_machine_range_not_fixnum_or_u64_range() {
        for (source, expected) in [
            (
                "(let ((integer-width 9223372036854775807)) (ash 1 128))",
                "340282366920938463463374607431768211456",
            ),
            (
                "(let ((integer-width -9223372036854775808)) (ash 1 128))",
                "340282366920938463463374607431768211456",
            ),
            (
                "(condition-case nil (let ((integer-width 9223372036854775808)) (ash 1 1)) (error 'caught))",
                "caught",
            ),
            (
                "(condition-case nil (let ((integer-width -9223372036854775809)) (ash 1 1)) (error 'caught))",
                "caught",
            ),
            (
                "(condition-case nil (let ((integer-width 18446744073709551615)) (expt 2 3)) (error 'caught))",
                "caught",
            ),
            (
                "(condition-case nil (let ((integer-width 1.0)) (ash 1 1)) (error 'caught))",
                "caught",
            ),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn exponentiation_uses_owned_numbers_and_dynamic_width() {
        for (source, expected) in [
            ("(expt 2 64)", "18446744073709551616"),
            ("(expt 2 -1)", "0.5"),
            ("(expt -2 -3)", "-0.125"),
            ("(expt 0 0)", "1"),
            ("(expt 0 9999999999999999999999999)", "0"),
            ("(expt 1 9999999999999999999999999)", "1"),
            ("(expt -1 9999999999999999999999999)", "-1"),
            (
                "(let ((integer-width 129)) (expt 2 128))",
                "340282366920938463463374607431768211456",
            ),
            (
                "(let ((integer-width 129)) (condition-case nil (expt 2 129) (error 'caught)))",
                "caught",
            ),
            (
                "(let ((integer-width 129)) (expt 3 81))",
                "443426488243037769948249630619149892803",
            ),
            (
                "(let ((integer-width 129)) (condition-case nil (expt 3 82) (error 'caught)))",
                "caught",
            ),
            (
                "(condition-case nil (let ((integer-width nil)) (expt 2.0 3)) (error 'caught))",
                "caught",
            ),
            ("(let ((integer-width -1)) (ash 1 -1))", "0"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn extrema_and_inequality_match_emacs_operand_rules() {
        for (source, expected) in [
            ("(integerp (max 1 1.0))", "t"),
            ("(integerp (min 1 1.0))", "t"),
            ("(floatp (max 1.0 1))", "t"),
            ("(floatp (min 1.0 1))", "t"),
            ("(= (max 0.0e+NaN 1 2) 2)", "nil"),
            ("(= (min 0.0e+NaN 1 2) 2)", "nil"),
            ("(copysign 1.0 (max 0.0e+NaN -0.0e+NaN))", "-1.0"),
            ("(copysign 1.0 (max -0.0e+NaN 0.0e+NaN))", "1.0"),
            ("(copysign 1.0 (min 0.0e+NaN -0.0e+NaN))", "-1.0"),
            ("(copysign 1.0 (min -0.0e+NaN 0.0e+NaN))", "1.0"),
            ("(/= 1 1.0)", "nil"),
            ("(/= 1 2)", "t"),
            ("(/= 0.0e+NaN 0.0e+NaN)", "t"),
            ("(condition-case nil (/= 1) (error 'caught))", "caught"),
            ("(condition-case nil (/= 1 2 2) (error 'caught))", "caught"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn test_max_min() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_eq!(
            max(cx.add_as(1.0), &[cx.add_as(2.1), cx.add_as(1.1), cx.add_as(1.0)]),
            NumberValue::Float(2.1)
        );
        assert_eq!(
            min(cx.add_as(1.1), &[cx.add_as(1.0), cx.add_as(2.1), cx.add_as(1.0)]),
            NumberValue::Float(1.0)
        );
    }

    #[test]
    fn test_other() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_eq!(logand(&[258_i64.into(), 255_i64.into()]).unwrap(), NumberValue::Int(2));
    }
}
