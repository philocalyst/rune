//! Lisp arithmetic adapters over the owned numeric model.
pub(crate) use crate::core::object::{MAX_FIXNUM, MIN_FIXNUM, NumberValue};
use crate::core::{
    env::Env,
    gc::{Context, Rt},
    object::{Number, Object},
};
use anyhow::Result;
#[cfg(test)]
use num_bigint::BigInt;
use rune_macros::defun;

pub(crate) fn add_value(vars: &[Number]) -> NumberValue {
    // Seed a nonempty sum from its first operand. A synthetic +0 loses
    // negative zero even in multi-operand sums such as (+ -0.0 -0.0).
    match vars.split_first() {
        Some((first, rest)) => rest.iter().fold(first.val(), |acc, x| acc + x.val()),
        None => NumberValue::Int(0),
    }
}

#[defun(name = "+")]
pub(crate) fn add<'ob>(
    vars: &[Number<'ob>],
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if let [one] = vars {
        Ok(one.as_obj_copy())
    } else {
        Ok(cx.add(finish_arithmetic(add_value(vars), env, cx)?))
    }
}

pub(crate) fn sub_value(number: Option<Number>, numbers: &[Number]) -> NumberValue {
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

pub(crate) fn mul_value(numbers: &[Number]) -> NumberValue {
    match numbers.split_first() {
        Some((first, rest)) => rest.iter().fold(first.val(), |acc, x| acc * x.val()),
        None => NumberValue::Int(1),
    }
}

#[defun(name = "*")]
pub(crate) fn mul<'ob>(
    numbers: &[Number<'ob>],
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if let [one] = numbers {
        Ok(one.as_obj_copy())
    } else {
        Ok(cx.add(finish_arithmetic(mul_value(numbers), env, cx)?))
    }
}

fn division_fold<'ob>(
    initial: NumberValue,
    floating: bool,
    mut denominators: impl Iterator<
        Item = std::result::Result<Number<'ob>, crate::core::error::TypeError>,
    >,
) -> Result<NumberValue> {
    let initial = if floating { NumberValue::Float(initial.to_float()) } else { initial };
    denominators.try_fold(initial, |value, denominator| -> Result<NumberValue> {
        let denominator = denominator?.val();
        let denominator =
            if floating { NumberValue::Float(denominator.to_float()) } else { denominator };
        Ok(value.checked_div(denominator)?)
    })
}

pub(crate) fn div_value(number: Number, divisors: &[Number]) -> Result<NumberValue> {
    let floating = number.is_float() || divisors.iter().any(Number::is_float);
    let denominators = if divisors.is_empty() { std::slice::from_ref(&number) } else { divisors };
    let initial = if divisors.is_empty() { NumberValue::Int(1) } else { number.val() };
    division_fold(initial, floating, denominators.iter().copied().map(Ok))
}

pub(crate) fn add_one_value(number: Number) -> NumberValue {
    number.val() + NumberValue::Int(1)
}

pub(crate) fn sub_one_value(number: Number) -> NumberValue {
    number.val() - NumberValue::Int(1)
}

/// Check only the final published arithmetic result. GNU permits temporary
/// over-width intermediates that cancel back into range in variadic folds.
/// This is not an allocation preflight: input/intermediate limb allocation is
/// still owned by num-bigint and may exceed the eventual result's policy.
pub(crate) fn finish_arithmetic(
    value: NumberValue,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<NumberValue> {
    value.check_integer_width(crate::data::integer_width(env, cx)?.max(128))?;
    Ok(value)
}

#[defun(name = "-")]
pub(crate) fn sub(
    number: Option<Number>,
    numbers: &[Number],
    env: &Rt<Env>,
    cx: &Context,
) -> Result<NumberValue> {
    finish_arithmetic(sub_value(number, numbers), env, cx)
}

#[defun(name = "/")]
pub(crate) fn div(
    number: Number,
    divisors: &[Object],
    env: &Rt<Env>,
    cx: &Context,
) -> Result<NumberValue> {
    let value = if divisors.is_empty() {
        div_value(number, &[])?
    } else {
        // Float selection scans tags, not numeric types. Without any float,
        // an earlier zero divisor must beat a later nonnumeric operand.
        let floating = number.is_float() || divisors.iter().any(Object::is_float);
        division_fold(number.val(), floating, divisors.iter().copied().map(Number::try_from))?
    };
    finish_arithmetic(value, env, cx)
}

#[defun(name = "1+")]
pub(crate) fn add_one(number: Number, env: &Rt<Env>, cx: &Context) -> Result<NumberValue> {
    finish_arithmetic(add_one_value(number), env, cx)
}

#[defun(name = "1-")]
pub(crate) fn sub_one(number: Number, env: &Rt<Env>, cx: &Context) -> Result<NumberValue> {
    finish_arithmetic(sub_one_value(number), env, cx)
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

// Convert operands one at a time: a later non-number must not mask an
// earlier float rejected by the integer operation. Lisp argument expressions
// have already all evaluated before this adapter is called.
pub(crate) fn logior_value(ints_or_markers: &[Object]) -> Result<NumberValue> {
    Ok(ints_or_markers.iter().try_fold(NumberValue::Int(0), |value, next| {
        value.bit_or(Number::try_from(*next)?.val()).map_err(anyhow::Error::from)
    })?)
}

fn logand_value(int_or_markers: &[Object]) -> Result<NumberValue> {
    Ok(int_or_markers.iter().try_fold(NumberValue::Int(-1), |value, next| {
        value.bit_and(Number::try_from(*next)?.val()).map_err(anyhow::Error::from)
    })?)
}

fn logxor_value(integers: &[Object]) -> Result<NumberValue> {
    Ok(integers.iter().try_fold(NumberValue::Int(0), |value, next| {
        value.bit_xor(Number::try_from(*next)?.val()).map_err(anyhow::Error::from)
    })?)
}

fn bitwise_object<'ob>(
    integers: &[Object<'ob>],
    operation: fn(&[Object]) -> Result<NumberValue>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    if let [one] = integers {
        let one = Number::try_from(*one)?;
        if one.is_float() {
            return Err(crate::core::object::NumericError::ExpectedInteger.into());
        }
        return Ok(one.as_obj_copy());
    }
    Ok(cx.add(finish_arithmetic(operation(integers)?, env, cx)?))
}

#[defun]
pub(crate) fn logior<'ob>(
    integers: &[Object<'ob>],
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    bitwise_object(integers, logior_value, env, cx)
}

#[defun]
fn logand<'ob>(integers: &[Object<'ob>], env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    bitwise_object(integers, logand_value, env, cx)
}

#[defun]
fn logxor<'ob>(integers: &[Object<'ob>], env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    bitwise_object(integers, logxor_value, env, cx)
}

#[defun]
fn lognot(integer: Number, env: &Rt<Env>, cx: &Context) -> Result<NumberValue> {
    finish_arithmetic(integer.val().bit_not()?, env, cx)
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
pub(crate) fn modulo(x: Number, y: Number, env: &Rt<Env>, cx: &Context) -> Result<NumberValue> {
    finish_arithmetic(remainder_driver(x, y, true)?, env, cx)
}

#[defun(name = "%")]
pub(crate) fn remainder(x: Number, y: Object, env: &Rt<Env>, cx: &Context) -> Result<NumberValue> {
    // GNU validates the first integer before converting the second operand.
    // TODO: Handle markers and named integer-or-marker type conditions.
    if x.is_float() {
        return Err(crate::core::object::NumericError::ExpectedInteger.into());
    }
    finish_arithmetic(remainder_driver(x, Number::try_from(y)?, false)?, env, cx)
}

// Extrema select an existing operand. Keep the borrowed identity rather than
// cloning its value and allocating a different float or bignum at the boundary.
fn extreme<'ob>(
    first: Number<'ob>,
    rest: &[Number<'ob>],
    preferred: std::cmp::Ordering,
) -> Number<'ob> {
    rest.iter()
        .copied()
        .fold(first, |selected, next| match selected.numeric_cmp(next) {
            Some(order) if order == preferred || order == std::cmp::Ordering::Equal => selected,
            Some(_) => next,
            None => {
                if matches!(next.untag(), crate::core::object::NumberType::Float(f) if f.is_nan()) {
                    next
                } else {
                    selected
                }
            }
        })
}

#[defun]
pub(crate) fn max<'ob>(
    number_or_marker: Number<'ob>,
    number_or_markers: &[Number<'ob>],
) -> Number<'ob> {
    extreme(number_or_marker, number_or_markers, std::cmp::Ordering::Greater)
}

#[defun]
pub(crate) fn min<'ob>(
    number_or_marker: Number<'ob>,
    number_or_markers: &[Number<'ob>],
) -> Number<'ob> {
    extreme(number_or_marker, number_or_markers, std::cmp::Ordering::Less)
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
        assert_eq!(add_value(&[]), NumberValue::Int(0));
        assert_eq!(add_value(&[7.into(), 13.into()]), NumberValue::Int(20));
        assert_eq!(add_value(&[1.into(), cx.add_as(2.5)]), NumberValue::Float(3.5));
        assert_eq!(add_value(&[0.into(), (-1).into()]), NumberValue::Int(-1));
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
        assert_eq!(sub_value(None, &[]), NumberValue::Int(0));
        assert_eq!(sub_value(Some(7.into()), &[]), NumberValue::Int(-7));
        assert_eq!(sub_value(Some(7.into()), &[13.into()]), NumberValue::Int(-6));
        assert_eq!(sub_value(Some(0.into()), &[(-1).into()]), NumberValue::Int(1));
    }

    #[test]
    fn test_mul() {
        assert_eq!(mul_value(&[]), NumberValue::Int(1));
        assert_eq!(mul_value(&[7.into(), 13.into()]), NumberValue::Int(91));
        assert_eq!(mul_value(&[(-1).into(), 1.into()]), NumberValue::Int(-1));
    }

    #[test]
    fn test_div() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);

        assert_eq!(div_value(cx.add_as(12.0), &[]).unwrap(), NumberValue::Float(1.0 / 12.0));
        assert_eq!(div_value(12.into(), &[5.into(), 2.into()]).unwrap(), NumberValue::Int(1));
    }

    #[test]
    fn m0_division_rejects_integer_zero_and_preserves_float_rules() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let zero = cx.add_as(BigInt::from(0));
        for dividend in [Number::from(-1073741824_i64), cx.add_as(BigInt::from(i64::MAX) + 1_u8)] {
            for denominator in [Number::from(0_i64), zero] {
                assert!(div_value(dividend, &[denominator]).is_err());
            }
        }
        assert!(div_value(0_i64.into(), &[]).is_err());
        assert_eq!(div_value(2_i64.into(), &[]).unwrap(), NumberValue::Int(0));
        assert_eq!(div_value((-1_i64).into(), &[]).unwrap(), NumberValue::Int(-1));
        assert_eq!(
            div_value(MIN_FIXNUM.into(), &[(-1_i64).into()]).unwrap(),
            NumberValue::Big(-BigInt::from(MIN_FIXNUM))
        );
        assert_eq!(
            div_value(10_i64.into(), &[3_i64.into(), cx.add_as(2.0)]).unwrap(),
            NumberValue::Float(10.0 / 3.0 / 2.0)
        );
        let NumberValue::Float(infinity) =
            div_value(2_i64.into(), &[0_i64.into(), cx.add_as(2.0)]).unwrap()
        else {
            panic!("any float operand must select floating division");
        };
        assert!(infinity.is_infinite());
        let NumberValue::Float(nan) = div_value(cx.add_as(0.0), &[0_i64.into()]).unwrap() else {
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
        for modulo in [false, true] {
            for numerator in [Number::from(5_i64), cx.add_as(BigInt::from(i64::MAX) + 1_u8)] {
                assert!(remainder_driver(numerator, zero, modulo).is_err());
            }
        }
        for (numerator, denominator) in
            [(cx.add_as(5.0), Number::from(0_i64)), (5_i64.into(), cx.add_as(-0.0))]
        {
            let NumberValue::Float(value) = remainder_driver(numerator, denominator, true).unwrap()
            else {
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
    fn division_prescans_float_tags_then_validates_operands_in_operation_order() {
        {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let context = &cx;
            rune_core::macros::root!(env, init(Env::new(context).unwrap()), context);
            let zero = Object::from(0_i64);
            let bad: Object = crate::sym::NIL.into();
            let float = cx.add(2.0);
            let error = div(1_i64.into(), &[zero, bad], env, &cx).unwrap_err();
            assert_eq!(
                error.downcast_ref::<crate::core::object::NumericError>(),
                Some(&crate::core::object::NumericError::DivisionByZero)
            );
            for divisors in
                [&[zero, bad, float][..], &[zero, float, bad][..], &[cx.add(0.0), bad][..]]
            {
                let error = div(1_i64.into(), divisors, env, &cx).unwrap_err();
                assert_eq!(
                    error.downcast_ref::<crate::core::error::TypeError>(),
                    Some(&crate::core::error::TypeError::new(
                        crate::core::error::Type::Number,
                        bad
                    ))
                );
            }
            let error = div(cx.add_as(1.0), &[zero, bad], env, &cx).unwrap_err();
            assert!(error.downcast_ref::<crate::core::error::TypeError>().is_some());
            let NumberValue::Float(infinity) = div(1_i64.into(), &[zero, float], env, &cx).unwrap()
            else {
                panic!("float expected")
            };
            assert_eq!(infinity, f64::INFINITY);
        }
        for (source, expected) in [
            (
                "(let ((seen 0)) (condition-case nil (/ 1 0 (progn (setq seen 1) 'bad)) (error nil)) seen)",
                "1",
            ),
            (
                "(let ((seen 0)) (condition-case nil (/ 1 0 (progn (setq seen 1) 'bad) (progn (setq seen 2) 2.0)) (error nil)) seen)",
                "2",
            ),
            ("(/ 1 0 2.0)", "1.0e+INF"),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn remainder_reports_first_float_before_later_nonnumeric_argument() {
        {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let context = &cx;
            rune_core::macros::root!(env, init(Env::new(context).unwrap()), context);
            let error = remainder(cx.add_as(1.0), crate::sym::NIL.into(), env, &cx).unwrap_err();
            assert_eq!(
                error.downcast_ref::<crate::core::object::NumericError>(),
                Some(&crate::core::object::NumericError::ExpectedInteger)
            );
        }
        crate::interpreter::assert_lisp(
            "(let ((seen 0)) (condition-case nil (% 1.0 (progn (setq seen 1) 'bad)) (error nil)) seen)",
            "1",
        );
    }

    #[test]
    fn remainder_and_modulo_validate_allocating_integer_results() {
        for (expression, expected) in [
            ("(% x d)", "caught"),
            ("(mod x d)", "caught"),
            ("(% nx d)", "caught"),
            ("(mod nx d)", "1"),
            ("(% x 3)", "1"),
            ("(mod x nx)", "0"),
            ("(% x x)", "0"),
            ("(mod x 1.0)", "0.0"),
        ] {
            crate::interpreter::assert_lisp(
                &format!(
                    "(let ((x (ash 1 200)) (nx (- (ash 1 200))) (d (1+ (ash 1 200)))) (let ((integer-width 128)) (condition-case nil {expression} (error 'caught))))"
                ),
                expected,
            );
        }
    }

    #[test]
    fn bitwise_width_is_final_and_singletons_retain_identity() {
        for (expression, expected) in [
            ("(logand x x)", "caught"),
            ("(logior x 0)", "caught"),
            ("(logxor x 0)", "caught"),
            ("(lognot x)", "caught"),
            ("(logand x x 0)", "0"),
            ("(logior x nx -1)", "-1"),
            ("(logxor x x)", "0"),
            ("(logxor x 0 x)", "0"),
            ("(eq x (logand x))", "t"),
            ("(eq x (logior x))", "t"),
            ("(eq x (logxor x))", "t"),
            ("(logand)", "-1"),
            ("(logior)", "0"),
            ("(logxor)", "0"),
            ("(logand 1.0)", "caught"),
            ("(logior 1.0)", "caught"),
            ("(logxor 1.0)", "caught"),
        ] {
            crate::interpreter::assert_lisp(
                &format!(
                    "(let ((x (ash 1 200)) (nx (- (ash 1 200)))) (let ((integer-width 128)) (condition-case nil {expression} (error 'caught))))"
                ),
                expected,
            );
        }
    }

    #[test]
    fn arithmetic_width_applies_to_final_results_not_temporary_fold_values() {
        for (expression, expected) in [
            ("(+ x 0)", "caught"),
            ("(+ x nx)", "0"),
            ("(+ y y ny)", "170141183460469231731687303715884105728"),
            ("(- x)", "caught"),
            ("(- x x)", "0"),
            ("(- x nx)", "caught"),
            ("(- y ny y)", "170141183460469231731687303715884105728"),
            ("(* x 1)", "caught"),
            ("(* x 0)", "0"),
            ("(* y y 0)", "0"),
            ("(/ x 1)", "caught"),
            ("(/ x x)", "1"),
            ("(1+ x)", "caught"),
            ("(1- x)", "caught"),
            ("(1+ nx)", "caught"),
            ("(1- nx)", "caught"),
            ("(floatp (+ x 0.0))", "t"),
            ("(floatp (* x 1.0))", "t"),
            ("(floatp (/ x 1.0))", "t"),
            ("(eq x (+ x))", "t"),
            ("(eq x (* x))", "t"),
        ] {
            crate::interpreter::assert_lisp(
                &format!(
                    "(let ((x (ash 1 200)) (nx (- (ash 1 200))) (y (ash 1 127)) (ny (- (ash 1 127)))) (let ((integer-width 128)) (condition-case nil {expression} (error 'caught))))"
                ),
                expected,
            );
        }
    }

    #[test]
    fn sums_and_products_preserve_zero_sign_without_synthetic_operands() {
        for length in 1..=4 {
            for mut encoded in 0..3_usize.pow(length) {
                let mut arguments = Vec::new();
                for _ in 0..length {
                    arguments.push(["0", "0.0", "-0.0"][encoded % 3]);
                    encoded /= 3;
                }
                if arguments.iter().all(|value| *value == "0") {
                    continue;
                }
                for operation in ["+", "*"] {
                    let negative = if operation == "+" {
                        arguments.iter().all(|value| *value == "-0.0")
                    } else {
                        arguments.iter().filter(|value| **value == "-0.0").count() % 2 != 0
                    };
                    crate::interpreter::assert_lisp(
                        &format!("(copysign 1.0 ({operation} {}))", arguments.join(" ")),
                        if negative { "-1.0" } else { "1.0" },
                    );
                }
            }
        }
    }

    #[test]
    fn singleton_sum_and_product_preserve_operand_identity_and_signed_zero() {
        for operation in ["+", "*"] {
            for value in ["1.5", "-0.0", "18446744073709551616", "0.0e+NaN", "-0.0e+NaN"] {
                crate::interpreter::assert_lisp(
                    &format!(
                        "(let ((x {value})) (let ((y ({operation} x))) (garbage-collect) (eq x y)))"
                    ),
                    "t",
                );
            }
        }
        crate::interpreter::assert_lisp("(copysign 1.0 (+ -0.0))", "-1.0");
        crate::interpreter::assert_lisp("(+)", "0");
        crate::interpreter::assert_lisp("(*)", "1");
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for operation in [add_value, mul_value] {
            let NumberValue::Float(value) = operation(&[cx.add_as(-0.0)]) else {
                panic!("float expected")
            };
            assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
        }
    }

    #[test]
    fn extrema_retain_the_selected_heap_operand() {
        for operation in ["max", "min"] {
            for value in ["1.5", "18446744073709551616", "0.0e+NaN", "-0.0e+NaN"] {
                crate::interpreter::assert_lisp(
                    &format!(
                        "(let ((x {value})) (let ((y ({operation} x))) (garbage-collect) (eq x y)))"
                    ),
                    "t",
                );
                crate::interpreter::assert_lisp(
                    &format!("(let ((x {value}) (y {value})) (eq x ({operation} x y)))"),
                    if value.contains("NaN") { "nil" } else { "t" },
                );
            }
            crate::interpreter::assert_lisp(
                &format!("(let ((x 0.0) (y -0.0)) (eq x ({operation} x y)))"),
                "t",
            );
        }
    }

    #[test]
    fn test_max_min() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_eq!(
            max(cx.add_as(1.0), &[cx.add_as(2.1), cx.add_as(1.1), cx.add_as(1.0)]).val(),
            NumberValue::Float(2.1)
        );
        assert_eq!(
            min(cx.add_as(1.1), &[cx.add_as(1.0), cx.add_as(2.1), cx.add_as(1.0)]).val(),
            NumberValue::Float(1.0)
        );
    }

    #[test]
    fn bitwise_validation_preserves_left_to_right_failure_order() {
        {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let float = cx.add(1.0);
            let symbol = crate::sym::NIL.into();
            for operation in [logand_value, logior_value, logxor_value] {
                let error = operation(&[float, symbol]).unwrap_err();
                assert!(matches!(
                    error.downcast_ref::<crate::core::object::NumericError>(),
                    Some(crate::core::object::NumericError::ExpectedInteger)
                ));
                // An absorbing zero still does not skip operand validation.
                assert!(operation(&[0_i64.into(), symbol]).is_err());
            }
        }
        for operation in ["logand", "logior", "logxor"] {
            crate::interpreter::assert_lisp(
                &format!(
                    "(let ((seen 0)) (condition-case nil ({operation} 1.0 (progn (setq seen 1) 'bad)) (error nil)) seen)"
                ),
                "1",
            );
        }
    }

    #[test]
    fn test_other() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_eq!(logand_value(&[258_i64.into(), 255_i64.into()]).unwrap(), NumberValue::Int(2));
    }
}
