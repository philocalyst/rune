//! Operations on floats.
use std::ops::{AddAssign, BitAnd, Div, Rem, SubAssign};

use crate::core::{
    cons::Cons,
    env::Env,
    gc::{Context, Rt},
    object::{LispFloat, Number, NumberType, NumberValue, Object},
};
use anyhow::Result;
use anyhow::anyhow;
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};

use rune_macros::defun;

#[inline(always)]
fn coerce(arg: Number) -> f64 {
    arg.to_float()
}

/* Return the integer exponent E such that D * FLT_RADIX**E (i.e.,
scalbn (D, E)) is an integer that has precision equal to D and is
representable as a double.

Return DBL_MANT_DIG - DBL_MIN_EXP (the maximum possible valid
scale) if D is zero or tiny.  Return one greater than that if
D is infinite, and two greater than that if D is a NaN.  */
fn double_integer_scale(d: f64) -> i64 {
    // frexp's exponent is not meaningful for nonfinite inputs. Preserve GNU's
    // sentinel scales so rescaling rejects them before integer conversion.
    let maximum = f64::MANTISSA_DIGITS as i64 - f64::MIN_EXP as i64;
    if d.is_nan() {
        return maximum + 2;
    }
    if d.is_infinite() {
        return maximum + 1;
    }
    if d == 0.0 {
        return maximum;
    }
    let exponent = frexp_f(d).1 - 1;

    if f64::MIN_EXP as i64 - 1 <= exponent && exponent < i64::MAX {
        f64::MANTISSA_DIGITS as i64 - 1 - exponent
    } else {
        let x = f64::MANTISSA_DIGITS as i64 - f64::MIN_EXP as i64;
        let c = if exponent == i64::MAX { 1 } else { 0 };
        if d.is_nan() { x + 2 } else { x + c }
    }
}

fn rescale_for_division(n: NumberValue, nscale: i64, dscale: i64) -> Result<BigInt> {
    let mut result = match n {
        NumberValue::Int(i) => BigInt::from(i),
        NumberValue::Float(f) => {
            if (f64::MANTISSA_DIGITS as i64 - f64::MIN_EXP as i64) < nscale {
                return Err(anyhow!("Overflow error"));
            }
            BigInt::from_f64(libm::scalbn(f, nscale as i32))
                .ok_or_else(|| anyhow!("Overflow error"))?
        }
        NumberValue::Big(b) => b,
    };

    if nscale < dscale {
        let power = (dscale - nscale) * f64::RADIX.ilog2() as i64;
        result *= BigInt::from(2).pow(power as u32);
    }

    Ok(result)
}

fn rounding_driver(
    n: NumberValue,
    d: Option<NumberValue>,
    double_round: fn(f64) -> f64,
    int_divide: fn(i64, i64) -> i64,
    bigum_divide: fn(BigInt, BigInt) -> BigInt,
) -> Result<NumberValue> {
    let d = match d {
        None => {
            return Ok(match n {
                NumberValue::Float(f) => {
                    let rounded = double_round(f);
                    if !rounded.is_finite() {
                        return Err(anyhow!("Overflow error"));
                    }
                    NumberValue::Float(rounded).try_truncate()?
                }
                other => other,
            });
        }
        Some(d) if d.is_zero() => {
            return Err(anyhow!("(arith-error)"));
        }
        Some(d) => d,
    };

    match (n, d) {
        // The owned domain includes every i64, regardless of today's narrower
        // immediate payload. Avoid MIN/-1 division and abs(MIN) in round2.
        (NumberValue::Int(n), NumberValue::Int(d)) if n != i64::MIN && d != i64::MIN => {
            Ok(NumberValue::Int(int_divide(n, d)))
        }
        (n, d) => {
            let dscale = match d {
                NumberValue::Float(f) => double_integer_scale(f),
                _ => 0,
            };

            let nscale = match n {
                NumberValue::Float(f) => double_integer_scale(f),
                _ => 0,
            };

            /* If the numerator is finite and the denominator infinite, the
            quotient is zero and there is no need to try the impossible task
            of rescaling the denominator.  */
            if dscale == f64::MANTISSA_DIGITS as i64 - f64::MIN_EXP as i64 + 1 && nscale < dscale {
                return Ok(NumberValue::Int(0));
            }

            let num = rescale_for_division(n, nscale, dscale)?;
            let denom = rescale_for_division(d, dscale, nscale)?;
            Ok(NumberValue::Big(bigum_divide(num, denom)).coerce_integer())
        }
    }
}

fn round2<T>(num: T, den: T) -> T
where
    T: Div<Output = T>
        + Rem<Output = T>
        + PartialOrd
        + num_traits::Zero
        + num_traits::Signed
        + BitAnd<Output = T>
        + AddAssign
        + SubAssign
        + Clone,
{
    // The C language's division operator gives us the remainder R
    // corresponding to truncated division, but we want the remainder R1
    // on the other side of 0 if R1 is closer to 0 than R is; because we
    // want to round to even, we also want R1 if R and R1 are the same
    // distance from 0 and if the truncated quotient is odd.
    let mut q: T = num.clone() / den.clone();
    let r: T = num.clone() % den.clone();
    let neg_d = den < T::zero();
    let neg_r = r < T::zero();
    let abs_r: T = r.abs();
    let abs_r1: T = den.abs() - abs_r.clone();

    let odd = (q.clone() & T::one()) == T::one();
    // At the midpoint only an odd truncated quotient needs adjusting. Keep
    // the comparison explicit rather than adding one to a bounded magnitude.
    if abs_r1 < abs_r || (abs_r1 == abs_r && odd) {
        if neg_d == neg_r {
            q += T::one();
        } else {
            q -= T::one();
        }
    }
    q
}

#[defun]
fn floor<'ob>(
    num: Number<'ob>,
    divisor: Option<Number>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if divisor.is_none() && !num.is_float() {
        return Ok(num.as_obj_copy());
    }
    let value = rounding_driver(
        num.val(),
        divisor.map(|d| d.val()),
        |f| f.floor(),
        |n, d| num_integer::Integer::div_floor(&n, &d),
        |n, d| n.div_floor(&d),
    )?;
    Ok(cx.add(crate::arith::finish_arithmetic(value, env, cx)?))
}

#[defun]
fn ceiling<'ob>(
    num: Number<'ob>,
    divisor: Option<Number>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if divisor.is_none() && !num.is_float() {
        return Ok(num.as_obj_copy());
    }
    let value = rounding_driver(
        num.val(),
        divisor.map(|d| d.val()),
        |f| f.ceil(),
        |n, d| num_integer::Integer::div_ceil(&n, &d),
        |n, d| n.div_ceil(&d),
    )?;
    Ok(cx.add(crate::arith::finish_arithmetic(value, env, cx)?))
}

#[defun]
fn round<'ob>(
    num: Number<'ob>,
    divisor: Option<Number>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if divisor.is_none() && !num.is_float() {
        return Ok(num.as_obj_copy());
    }
    let value = rounding_driver(
        num.val(),
        divisor.map(|d| d.val()), //
        // Use the same even-midpoint policy as the exact divisor path.
        |f| f.round_ties_even(),
        round2,
        round2,
    )?;
    Ok(cx.add(crate::arith::finish_arithmetic(value, env, cx)?))
}

#[defun]
fn truncate<'ob>(
    num: Number<'ob>,
    divisor: Option<Number>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if divisor.is_none() && !num.is_float() {
        return Ok(num.as_obj_copy());
    }
    let value = rounding_driver(
        num.val(),
        divisor.map(|d| d.val()), //
        |f| f,
        |n, d| n.div(&d),
        |n, d| n.div(&d),
    )?;
    Ok(cx.add(crate::arith::finish_arithmetic(value, env, cx)?))
}

#[defun]
fn fceiling(num: &LispFloat) -> f64 {
    num.ceil()
}

#[defun]
fn ffloor(num: &LispFloat) -> f64 {
    num.floor()
}

#[defun]
fn fround(num: &LispFloat) -> f64 {
    num.round_ties_even()
}

#[defun]
fn ftruncate(num: &LispFloat) -> f64 {
    num.trunc()
}

#[defun]
fn float<'ob>(arg: Number<'ob>, cx: &'ob Context) -> Object<'ob> {
    if arg.is_float() { arg.as_obj_copy() } else { cx.add(coerce(arg)) }
}

#[defun]
fn asin(arg: Number) -> f64 {
    coerce(arg).asin()
}

#[defun]
fn acos(arg: Number) -> f64 {
    coerce(arg).acos()
}

#[defun]
fn atan(arg: Number, x: Option<f64>) -> f64 {
    if let Some(x) = x { coerce(arg).atan2(x) } else { coerce(arg).atan() }
}

#[defun]
fn cos(arg: Number) -> f64 {
    coerce(arg).cos()
}

#[defun]
fn sin(arg: Number) -> f64 {
    coerce(arg).sin()
}

#[defun]
fn tan(arg: Number) -> f64 {
    coerce(arg).tan()
}

#[defun]
fn isnan(arg: &LispFloat) -> bool {
    arg.is_nan()
}

#[defun]
fn copysign<'ob>(x: &'ob LispFloat, y: &LispFloat, cx: &'ob Context) -> Object<'ob> {
    if x.is_sign_negative() == y.is_sign_negative() {
        cx.add(x)
    } else {
        cx.add(-**x)
    }
}

#[defun]
fn exp(arg: Number) -> f64 {
    coerce(arg).exp()
}

#[defun]
fn expt(
    x: Number,
    y: Number,
    env: &crate::core::gc::Rt<crate::core::env::Env>,
    cx: &Context,
) -> Result<NumberValue> {
    Ok(x.val().checked_pow(y.val(), crate::data::integer_width(env, cx)?)?)
}

#[defun]
fn log(arg: Number, base: Option<f64>) -> f64 {
    // GNU uses dedicated libm operations for the common bases, not a ratio
    // of natural logs. The latter loses exact powers (e.g. log(1000, 10)).
    let value = coerce(arg);
    match base {
        None => value.ln(),
        Some(2.0) => value.log2(),
        Some(10.0) => value.log10(),
        Some(base) => value.ln() / base.ln(),
    }
}

#[defun]
fn sqrt(arg: Number) -> f64 {
    coerce(arg).sqrt()
}

#[defun]
fn abs<'ob>(arg: Number<'ob>, env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    // GNU returns the original heap number when its sign needs no change.
    // Besides avoiding allocation, this is observable through Lisp `eq`.
    Ok(match arg.untag() {
        NumberType::Int(i) if i < 0 => {
            cx.add(crate::arith::finish_arithmetic(NumberValue::Int(-i), env, cx)?)
        }
        NumberType::Float(f) if f.is_sign_negative() => cx.add(-**f),
        NumberType::Big(b) if b.is_negative() => {
            cx.add(crate::arith::finish_arithmetic(NumberValue::Big(-(**b).clone()), env, cx)?)
        }
        _ => arg.as_obj_copy(),
    })
}

#[defun]
fn ldexp(significand: Object, exponent: Object) -> Result<f64> {
    // Fldexp validates its fixnum exponent before coercing the significand.
    let exponent: i64 = exponent.try_into()?;
    Ok(ldexp_value(Number::try_from(significand)?, exponent))
}

fn ldexp_value(s: Number, e: i64) -> f64 {
    // Match GNU Fldexp's C-int exponent clamp. Scaling directly avoids both
    // wrapping casts and an overflowing/underflowing intermediate power of 2.
    let exponent = e.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    libm::scalbn(coerce(s), exponent)
}

#[defun]
fn logb<'ob>(arg: Number<'ob>, cx: &'ob Context) -> Object<'ob> {
    // GNU returns nonnegative infinity and every NaN unchanged. Do not route
    // these through owned conversion, which would lose observable identity.
    if matches!(arg.untag(), NumberType::Float(f) if f.is_nan() || **f == f64::INFINITY) {
        return arg.as_obj_copy();
    }
    cx.add(logb_value(arg))
}

fn logb_value(arg: Number) -> NumberValue {
    // GNU Flogb uses magnitude and exact integer bit widths, not a rounded
    // floating logarithm (which loses powers near large integer boundaries).
    match arg.untag() {
        NumberType::Int(0) => NumberValue::Float(f64::NEG_INFINITY),
        NumberType::Int(value) => NumberValue::Int(i64::from(value.unsigned_abs().ilog2())),
        NumberType::Big(value) if value.is_zero() => NumberValue::Float(f64::NEG_INFINITY),
        NumberType::Big(value) => NumberValue::Big(BigInt::from(value.bits() - 1)).coerce_integer(),
        NumberType::Float(value) => {
            let value = **value;
            if value == 0.0 {
                NumberValue::Float(f64::NEG_INFINITY)
            } else if value.is_nan() {
                NumberValue::Float(value)
            } else if value.is_infinite() {
                NumberValue::Float(f64::INFINITY)
            } else {
                NumberValue::Int(frexp_f(value).1 - 1)
            }
        }
    }
}

#[defun]
fn frexp<'ob>(x: Number, cx: &'ob Context) -> Object<'ob> {
    let f = coerce(x);
    let (significand, exponent) = frexp_f(f);
    Cons::new(significand, exponent, cx).into()
}

fn frexp_f(f: f64) -> (f64, i64) {
    let (x, exp) = libm::frexp(f);
    (x, exp as i64)
}

#[cfg(test)]
mod m0_rounding_regressions {
    use super::*;
    use crate::interpreter::assert_lisp;

    #[test]
    fn owned_integer_rounding_covers_full_signed_machine_domain_without_panics() {
        type MachineDivide = fn(i64, i64) -> i64;
        type BigDivide = fn(BigInt, BigInt) -> BigInt;
        let operations: [(fn(f64) -> f64, MachineDivide, BigDivide); 4] = [
            (f64::floor, |n, d| n.div_floor(&d), |n, d| n.div_floor(&d)),
            (f64::ceil, |n, d| n.div_ceil(&d), |n, d| n.div_ceil(&d)),
            (f64::round_ties_even, round2, round2),
            (f64::trunc, |n, d| n / d, |n, d| n / d),
        ];
        for (float_round, machine_divide, big_divide) in operations {
            for numerator in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX] {
                for denominator in [i64::MIN, -2, -1, 1, 2, i64::MAX] {
                    let expected = big_divide(BigInt::from(numerator), BigInt::from(denominator));
                    let actual = rounding_driver(
                        NumberValue::Int(numerator),
                        Some(NumberValue::Int(denominator)),
                        float_round,
                        machine_divide,
                        big_divide,
                    )
                    .unwrap();
                    assert_eq!(actual.into_integer().unwrap(), expected);
                }
            }
        }
    }

    #[test]
    fn allocating_rounding_and_absolute_value_obey_integer_width() {
        for operation in ["floor", "ceiling", "round", "truncate"] {
            for (args, expected) in [
                ("x 1", "caught"),
                ("nx 1", "caught"),
                ("x x", "1"),
                ("1.0e+100", "caught"),
                ("1.0e+100 1.0e+100", "1"),
            ] {
                assert_lisp(
                    &format!(
                        "(let ((x (ash 1 200)) (nx (- (ash 1 200)))) (let ((integer-width 128)) (condition-case nil ({operation} {args}) (error 'caught))))"
                    ),
                    expected,
                );
            }
            assert_lisp(
                &format!(
                    "(let ((x (ash 1 200))) (let ((integer-width 128)) (eq x ({operation} x))))"
                ),
                "t",
            );
        }
        assert_lisp("(let ((x (ash 1 200))) (let ((integer-width 128)) (eq x (abs x))))", "t");
        assert_lisp(
            "(let ((x (- (ash 1 200)))) (let ((integer-width 128)) (condition-case nil (abs x) (error 'caught))))",
            "caught",
        );
        assert_lisp("(let ((integer-width 128)) (abs -1.0e+100))", "1e+100");
    }

    #[test]
    fn unary_integer_rounding_preserves_existing_bignum_objects() {
        for operation in ["floor", "ceiling", "round", "truncate"] {
            for value in ["18446744073709551616", "-18446744073709551616"] {
                for divisor in ["", " nil"] {
                    assert_lisp(
                        &format!(
                            "(let ((x {value})) (let ((y ({operation} x{divisor}))) (garbage-collect) (eq x y)))"
                        ),
                        "t",
                    );
                }
            }
        }
    }

    #[test]
    fn sign_preserving_primitives_retain_heap_number_identity() {
        for source in [
            "(let ((x 1.5)) (eq x (float x)))",
            "(let ((x -0.0)) (eq x (float x)))",
            "(let ((x 0.0e+NaN)) (eq x (float x)))",
            "(let ((x 1.5)) (eq x (abs x)))",
            "(let ((x 0.0e+NaN)) (eq x (abs x)))",
            "(let ((x 18446744073709551616)) (eq x (abs x)))",
            "(let ((x 1.5)) (eq x (copysign x 1.0)))",
            "(let ((x -0.0)) (eq x (copysign x -1.0)))",
            "(let ((x -0.0e+NaN)) (eq x (copysign x -1.0)))",
            "(let ((x -0.0)) (if (eq x (abs x)) nil t))",
            "(let ((x -1.5)) (if (eq x (abs x)) nil t))",
            "(let ((x -18446744073709551616)) (if (eq x (abs x)) nil t))",
            "(let ((x 1.5)) (if (eq x (copysign x -1.0)) nil t))",
            "(let ((x 1.5)) (let ((y (float x))) (garbage-collect) (eq x y)))",
        ] {
            assert_lisp(source, "t");
        }
    }

    #[test]
    fn logarithms_recover_all_finite_binary_powers() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for exponent in -1074..=1023 {
            let value = libm::scalbn(1.0, exponent);
            assert_eq!(log(cx.add_as(value), Some(2.0)), f64::from(exponent));
        }
        for exponent in -307..=308 {
            let value: f64 = format!("1e{exponent}").parse().unwrap();
            assert_eq!(log(cx.add_as(value), Some(10.0)), f64::from(exponent));
        }
    }

    #[test]
    fn logarithms_use_dedicated_common_base_operations() {
        for (source, expected) in [
            ("(log 1000 10)", "3.0"),
            ("(log 1e-300 10)", "-300.0"),
            ("(log 8 2)", "3.0"),
            ("(log 0.125 2)", "-3.0"),
            ("(log 5e-324 2)", "-1074.0"),
            ("(log 1)", "0.0"),
            ("(log 0)", "-1.0e+INF"),
            ("(isnan (log -1))", "t"),
            ("(log 16 4)", "2.0"),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn unary_rounding_uses_the_same_even_midpoints_as_division() {
        for (value, expected) in [
            (0.5_f64, 0.0_f64),
            (1.5, 2.0),
            (2.5, 2.0),
            (3.5, 4.0),
            (-0.5, -0.0),
            (-1.5, -2.0),
            (-2.5, -2.0),
            (-3.5, -4.0),
        ] {
            let value = LispFloat::new(value, false);
            assert_eq!(fround(&value).to_bits(), expected.to_bits());
        }
        for (source, expected) in [
            ("(round 2.5)", "2"),
            ("(round 3.5)", "4"),
            ("(round -2.5)", "-2"),
            ("(round -3.5)", "-4"),
            ("(fround 2.5)", "2.0"),
            ("(fround -2.5)", "-2.0"),
            ("(= (round 2.5) (round 5 2))", "t"),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn float_only_primitives_reject_integer_coercion() {
        for (source, expected) in [
            ("(condition-case nil (fround 1) (error 'caught))", "caught"),
            ("(condition-case nil (fceiling 1) (error 'caught))", "caught"),
            ("(condition-case nil (ffloor 1) (error 'caught))", "caught"),
            ("(condition-case nil (ftruncate 1) (error 'caught))", "caught"),
            ("(condition-case nil (copysign 1 1.0) (error 'caught))", "caught"),
            ("(condition-case nil (copysign 1.0 1) (error 'caught))", "caught"),
            ("(condition-case nil (isnan 1) (error 'caught))", "caught"),
            ("(condition-case nil (fround 18446744073709551616) (error 'caught))", "caught"),
            ("(fround 1.5)", "2.0"),
            ("(copysign 1.0 -1.0)", "-1.0"),
            ("(isnan 0.0e+NaN)", "t"),
            ("(atan 0 18446744073709551616)", "0.0"),
            ("(log 18446744073709551616 18446744073709551616)", "1.0"),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn integer_divisor_rounding_uses_even_midpoints() {
        for (numerator, denominator, expected) in [
            (5_i64, 2_i64, 2_i64),
            (7, 2, 4),
            (-5, 2, -2),
            (-7, 2, -4),
            (5, -2, -2),
            (7, -2, -4),
            (-5, -2, 2),
            (-7, -2, 4),
            (4, 3, 1),
            (5, 3, 2),
            (-4, 3, -1),
            (-5, 3, -2),
            (6, 3, 2),
        ] {
            assert_eq!(round2(numerator, denominator), expected);
            assert_eq!(
                round2(BigInt::from(numerator), BigInt::from(denominator)),
                BigInt::from(expected)
            );
        }
    }

    #[test]
    fn round_primitive_preserves_even_midpoints_across_numeric_types() {
        for (source, expected) in [
            ("(round 5 2)", "2"),
            ("(round 7 2)", "4"),
            ("(round -5 2)", "-2"),
            ("(round -7 -2)", "4"),
            ("(round 5.0 2)", "2"),
            ("(round 7 2.0)", "4"),
            ("(round 18446744073709551617 2)", "9223372036854775808"),
            ("(round 18446744073709551619 2)", "9223372036854775810"),
        ] {
            assert_lisp(source, expected);
        }
    }
}

#[cfg(test)]
mod m0_nonfinite_rounding_regressions {
    use super::*;
    use crate::{
        core::gc::{Context, RootSet},
        interpreter::assert_lisp,
    };

    #[test]
    fn integer_rounding_rejects_nonfinite_values_without_panicking() {
        {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let context = &cx;
            rune_core::macros::root!(env, init(Env::new(context).unwrap()), context);
            for operation in [floor, ceiling, round, truncate] {
                for value in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                    let value = cx.add_as(value);
                    assert!(operation(value, None, env, &cx).is_err());
                    assert!(operation(value, Some(1_i64.into()), env, &cx).is_err());
                    if matches!(value.untag(), NumberType::Float(f) if f.is_nan()) {
                        assert!(operation(1_i64.into(), Some(value), env, &cx).is_err());
                    }
                }
                for infinity in [f64::INFINITY, f64::NEG_INFINITY] {
                    assert_eq!(
                        operation(5_i64.into(), Some(cx.add_as(infinity)), env, &cx).unwrap(),
                        Object::from(0_i64)
                    );
                    assert!(
                        operation(cx.add_as(infinity), Some(cx.add_as(infinity)), env, &cx)
                            .is_err()
                    );
                }
            }
        }
        assert_lisp("(condition-case nil (floor 1.0e+INF) (error 'caught))", "caught");
        assert_lisp("(condition-case nil (truncate 1.0e+INF 1) (error 'caught))", "caught");
        assert_lisp("(floor 5 1.0e+INF)", "0");
        assert_lisp("(condition-case nil (ceiling 5 0.0e+NaN) (error 'caught))", "caught");
    }
}

#[cfg(test)]
mod m0_ldexp_regressions {
    use super::*;
    use crate::{
        core::gc::{Context, RootSet},
        interpreter::assert_lisp,
    };

    #[test]
    fn ldexp_validates_exponent_before_significand_after_argument_evaluation() {
        {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let exponent = cx.add(1.5);
            let error = ldexp(crate::sym::NIL.into(), exponent).unwrap_err();
            assert_eq!(
                error.downcast_ref::<crate::core::error::TypeError>(),
                Some(&crate::core::error::TypeError::new(crate::core::error::Type::Int, exponent))
            );
        }
        assert_lisp(
            "(let ((seen 0)) (condition-case nil (ldexp (progn (setq seen 1) 'bad) (progn (setq seen 2) 'bad2)) (error nil)) seen)",
            "2",
        );
    }

    #[test]
    fn ldexp_scales_without_intermediate_overflow_or_wrapping_exponents() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let half = cx.add_as(0.5);
        assert_eq!(ldexp_value(half, 1024), f64::from_bits(0x7fe0_0000_0000_0000));
        assert_eq!(ldexp_value(cx.add_as(1.0), -1074), f64::from_bits(1));
        assert!(ldexp_value(half, 1_i64 << 32).is_infinite());
        assert_eq!(ldexp_value(half, -(1_i64 << 32)), 0.0);
        for exponent in [i64::MIN, i64::MAX] {
            let zero = ldexp_value(cx.add_as(-0.0), exponent);
            assert_eq!(zero, 0.0);
            assert!(zero.is_sign_negative());
            assert_eq!(ldexp_value(cx.add_as(f64::INFINITY), exponent), f64::INFINITY);
        }
        assert!(ldexp_value(cx.add_as(f64::NAN), 100).is_nan());
        drop(cx);
        assert_lisp("(ldexp 0.5 4)", "8.0");
        assert_lisp("(ldexp 0.5 4294967296)", "1.0e+INF");
        assert_lisp("(ldexp 0.5 -4294967296)", "0.0");
    }
}

#[cfg(test)]
mod m0_logb_regressions {
    use super::*;
    use crate::{
        core::gc::{Context, RootSet},
        interpreter::assert_lisp,
    };

    #[test]
    fn logb_preserves_nonfinite_operand_identity_and_payload() {
        for literal in ["1.0e+INF", "0.0e+NaN", "1.0e+NaN", "-1.0e+NaN"] {
            assert_lisp(
                &format!("(let ((x {literal})) (let ((y (logb x))) (garbage-collect) (eq x y)))"),
                "t",
            );
        }
        assert_lisp("(logb -1.0e+INF)", "1.0e+INF");
        assert_lisp("(let ((x -1.0e+INF)) (eq x (logb x)))", "nil");
        assert_lisp("(eql (logb -1.0e+NaN) -1.0e+NaN)", "t");
    }

    #[test]
    fn logb_uses_magnitude_exact_integer_width_and_ieee_special_values() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (number, expected) in [
            (Number::from(-8_i64), 3_i64),
            (cx.add_as(-0.125), -3),
            (cx.add_as(f64::from_bits(1)), -1074),
            (cx.add_as(f64::MAX), 1023),
            (cx.add_as(BigInt::from(1_u8) << 4096_usize), 4096),
            (cx.add_as((BigInt::from(1_u8) << 4096_usize) - 1_u8), 4095),
            (cx.add_as(-((BigInt::from(1_u8) << 4096_usize) - 1_u8)), 4095),
        ] {
            assert_eq!(logb_value(number), NumberValue::Int(expected));
        }
        for number in [Number::from(0_i64), cx.add_as(0.0), cx.add_as(-0.0)] {
            assert_eq!(logb_value(number), NumberValue::Float(f64::NEG_INFINITY));
        }
        for infinity in [f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(logb_value(cx.add_as(infinity)), NumberValue::Float(f64::INFINITY));
        }
        let NumberValue::Float(nan) = logb_value(cx.add_as(f64::NAN)) else {
            panic!("NaN exponent must remain a float");
        };
        assert!(nan.is_nan());
        drop(cx);
        assert_lisp("(logb -8)", "3");
        assert_lisp("(logb 0)", "-1.0e+INF");
        assert_lisp("(logb -1.0e+INF)", "1.0e+INF");
        assert_lisp("(logb 18446744073709551615)", "63");
    }
}
