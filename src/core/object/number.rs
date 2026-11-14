//! Owned numeric values and the canonical object boundary.
//!
//! This module deliberately knows neither Lisp argument lists nor evaluation.
//! Primitive adapters use its fallible operations; the collector still owns
//! allocation through `Block`. The storage bounds are isolated here for R1's
//! later low-bit-tag migration. They describe the *current* encoding, not GNU's
//! target 62-bit payload.
use super::{Gc, IntoObject, Number, NumberType, Object, ObjectType};
use crate::core::gc::{Block, Context};
use num_bigint::{BigInt, Sign};
use num_traits::{FromPrimitive, ToPrimitive, Zero};
use std::{
    cmp::Ordering,
    fmt,
    ops::{Add, Mul, Neg, Sub},
};

pub(crate) const FIXNUM_PAYLOAD_BITS: u32 = 56;
pub(crate) const MAX_FIXNUM: i64 = (1_i64 << (FIXNUM_PAYLOAD_BITS - 1)) - 1;
pub(crate) const MIN_FIXNUM: i64 = -(1_i64 << (FIXNUM_PAYLOAD_BITS - 1));

/// Checked immediate-integer constructor. Unlike the legacy `i64::tag`, this
/// type can never contain an unrepresentable immediate payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fixnum(i64);
impl Fixnum {
    pub(crate) fn new(value: i64) -> Option<Self> {
        (MIN_FIXNUM..=MAX_FIXNUM).contains(&value).then_some(Self(value))
    }
    pub(crate) const fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NumericError {
    DivisionByZero,
    ExpectedInteger,
    NonFiniteInteger,
    IntegerWidthOutOfRange,
    IntegerWidth { bits: u64, limit: u64 },
}
impl fmt::Display for NumericError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DivisionByZero => f.write_str("(arith-error)"),
            Self::ExpectedInteger => f.write_str("expected integer argument"),
            Self::NonFiniteInteger => f.write_str("cannot convert non-finite float to integer"),
            Self::IntegerWidthOutOfRange => {
                f.write_str("(overflow-error) integer-width must fit a signed 64-bit integer")
            }
            Self::IntegerWidth { bits, limit } => {
                write!(f, "integer width {bits} exceeds limit {limit}")
            }
        }
    }
}
impl std::error::Error for NumericError {}

/// A GC-independent number. Variants retain their established spellings during
/// migration; `normalize` and `IntoObject` enforce canonical integer storage.
#[derive(Debug, Clone)]
pub(crate) enum NumberValue {
    Int(i64),
    Float(f64),
    Big(BigInt),
}

#[derive(Clone, Copy)]
enum NumericRef<'a> {
    Int(i64),
    Float(f64),
    Big(&'a BigInt),
}

impl<'ob> Number<'ob> {
    fn numeric_ref(self) -> NumericRef<'ob> {
        match self.untag() {
            NumberType::Int(value) => NumericRef::Int(value),
            NumberType::Float(value) => NumericRef::Float(**value),
            NumberType::Big(value) => NumericRef::Big(value),
        }
    }
    pub(crate) fn numeric_cmp(self, rhs: Number) -> Option<Ordering> {
        compare_refs(self.numeric_ref(), rhs.numeric_ref())
    }
    pub(crate) fn to_float(self) -> f64 {
        match self.numeric_ref() {
            NumericRef::Int(value) => value as f64,
            NumericRef::Float(value) => value,
            NumericRef::Big(value) => bigint_float(value),
        }
    }
    /// Decode the forwarded policy value without cloning a potentially huge
    /// BigInt only to discover that it cannot fit the signed machine domain.
    pub(crate) fn integer_width(self) -> Result<u64, NumericError> {
        match self.numeric_ref() {
            NumericRef::Int(value) => Ok(value as u64),
            NumericRef::Big(value) => value
                .to_i64()
                .map(|value| value as u64)
                .ok_or(NumericError::IntegerWidthOutOfRange),
            NumericRef::Float(_) => Err(NumericError::ExpectedInteger),
        }
    }
    pub(crate) fn val(self) -> NumberValue {
        match self.untag() {
            NumberType::Int(value) => NumberValue::Int(value),
            NumberType::Float(value) => NumberValue::Float(**value),
            NumberType::Big(value) => NumberValue::Big((**value).clone()),
        }
    }
}
impl NumberValue {
    /// GNU extrema keep the first equal operand (including its numeric kind
    /// and signed zero), and propagate NaNs, with a later NaN taking precedence.
    fn extreme(self, rhs: Self, preferred: Ordering) -> Self {
        match self.partial_cmp(&rhs) {
            Some(order) if order == preferred || order == Ordering::Equal => self,
            None => {
                if matches!(&rhs, Self::Float(value) if value.is_nan()) {
                    rhs
                } else {
                    self
                }
            }
            _ => rhs,
        }
    }
    pub(crate) fn maximum(self, rhs: Self) -> Self {
        self.extreme(rhs, Ordering::Greater)
    }
    pub(crate) fn minimum(self, rhs: Self) -> Self {
        self.extreme(rhs, Ordering::Less)
    }
    fn numeric_ref(&self) -> NumericRef<'_> {
        match self {
            Self::Int(value) => NumericRef::Int(*value),
            Self::Float(value) => NumericRef::Float(*value),
            Self::Big(value) => NumericRef::Big(value),
        }
    }
    pub(crate) fn integer(value: impl Into<BigInt>) -> Self {
        let value = value.into();
        match value.to_i64().and_then(Fixnum::new) {
            Some(value) => Self::Int(value.get()),
            None => Self::Big(value),
        }
    }
    pub(crate) fn normalize(self) -> Self {
        match self {
            Self::Int(value) if Fixnum::new(value).is_none() => Self::Big(value.into()),
            Self::Big(value) => Self::integer(value),
            other => other,
        }
    }
    pub(crate) fn to_float(&self) -> f64 {
        match self {
            Self::Int(value) => *value as f64,
            Self::Float(value) => *value,
            Self::Big(value) => bigint_float(value),
        }
    }
    pub(crate) fn into_integer(self) -> Result<BigInt, NumericError> {
        match self {
            Self::Int(value) => Ok(value.into()),
            Self::Big(value) => Ok(value),
            Self::Float(_) => Err(NumericError::ExpectedInteger),
        }
    }
    /// GNU's forwarded integer-width variable stores a signed machine integer,
    /// not merely a fixnum. Its arithmetic limit uses the unsigned bit pattern.
    pub(crate) fn into_integer_width(self) -> Result<u64, NumericError> {
        let value = self.into_integer()?;
        let value = value.to_i64().ok_or(NumericError::IntegerWidthOutOfRange)?;
        Ok(value as u64)
    }
    pub(crate) fn try_truncate(self) -> Result<Self, NumericError> {
        match self {
            Self::Float(value) => {
                BigInt::from_f64(value).map(Self::integer).ok_or(NumericError::NonFiniteInteger)
            }
            other => Ok(other.normalize()),
        }
    }
    /// Transitional finite-conversion adapter for existing Rust callers. A
    /// nonfinite value remains a float, never silently becomes integer zero.
    pub(crate) fn coerce_integer(self) -> Self {
        match self {
            Self::Float(value) => Self::Float(value).try_truncate().unwrap_or(Self::Float(value)),
            other => other.normalize(),
        }
    }
    /// Policy is explicit rather than a hidden allocation cutoff. Callers must
    /// choose the Lisp runtime's integer-width; this checks an existing result.
    pub(crate) fn check_integer_width(&self, limit: u64) -> Result<(), NumericError> {
        let bits = match self {
            Self::Int(value) => u64::from(64 - value.unsigned_abs().leading_zeros()),
            Self::Big(value) => value.bits(),
            Self::Float(_) => return Ok(()),
        };
        if bits > limit {
            Err(NumericError::IntegerWidth { bits, limit })
        } else {
            Ok(())
        }
    }
    pub(crate) fn bit_and(self, rhs: Self) -> Result<Self, NumericError> {
        if let (Self::Int(left), Self::Int(right)) = (&self, &rhs) {
            return Ok(Self::Int(left & right).normalize());
        }
        Ok(Self::integer(self.into_integer()? & rhs.into_integer()?))
    }
    pub(crate) fn bit_or(self, rhs: Self) -> Result<Self, NumericError> {
        if let (Self::Int(left), Self::Int(right)) = (&self, &rhs) {
            return Ok(Self::Int(left | right).normalize());
        }
        Ok(Self::integer(self.into_integer()? | rhs.into_integer()?))
    }
    pub(crate) fn bit_xor(self, rhs: Self) -> Result<Self, NumericError> {
        if let (Self::Int(left), Self::Int(right)) = (&self, &rhs) {
            return Ok(Self::Int(left ^ right).normalize());
        }
        Ok(Self::integer(self.into_integer()? ^ rhs.into_integer()?))
    }
    pub(crate) fn bit_not(self) -> Result<Self, NumericError> {
        if let Self::Int(value) = self {
            return Ok(Self::Int(!value).normalize());
        }
        Ok(Self::integer(!self.into_integer()?))
    }
    /// Count one bits for nonnegative values and zero bits for negative values
    /// in their infinite two's-complement representation.
    pub(crate) fn bit_count(self) -> Result<Self, NumericError> {
        if let Self::Int(value) = self {
            let magnitude = if value < 0 { !value } else { value };
            return Ok(Self::Int(i64::from(magnitude.count_ones())));
        }
        let value = self.into_integer()?;
        let value = if value.sign() == Sign::Minus { !value } else { value };
        let count: u64 = value
            .magnitude()
            .iter_u64_digits()
            .map(|limb| u64::from(limb.count_ones()))
            .sum();
        Ok(Self::integer(count))
    }
    /// Exact nonnegative integer powers with allocation bounded by the Lisp
    /// numeric policy. Negative exponents and float operands select IEEE pow.
    pub(crate) fn checked_pow(
        self,
        exponent: Self,
        integer_width: u64,
    ) -> Result<Self, NumericError> {
        if matches!(self, Self::Float(_)) || matches!(exponent, Self::Float(_)) {
            return Ok(Self::Float(self.to_float().powf(exponent.to_float())));
        }
        if let (Self::Int(base), Self::Int(power)) = (&self, &exponent) {
            if *power >= 0 {
                let result = match *base {
                    0 => Some(i64::from(*power == 0)),
                    1 => Some(1),
                    -1 => Some(if *power & 1 == 0 { 1 } else { -1 }),
                    _ => u32::try_from(*power).ok().and_then(|power| base.checked_pow(power)),
                };
                if let Some(result) = result {
                    // Every machine result is below the effective 128-bit
                    // minimum width policy, but still needs storage normalization.
                    return Ok(Self::Int(result).normalize());
                }
            }
        }
        let exponent = exponent.into_integer()?;
        if exponent.sign() == Sign::Minus {
            return Ok(Self::Float(self.to_float().powf(bigint_float(&exponent))));
        }
        let mut base = self.into_integer()?;
        if exponent.is_zero() {
            return Ok(Self::Int(1));
        }
        // These remain exact even when the exponent cannot fit a machine word.
        if base.is_zero() || base == BigInt::from(1) {
            return Ok(Self::integer(base));
        }
        if base == BigInt::from(-1) {
            return Ok(Self::Int(if (&exponent & BigInt::from(1)).is_zero() { 1 } else { -1 }));
        }
        let limit = integer_width.max(128);
        let mut exponent =
            exponent.to_u64().ok_or(NumericError::IntegerWidth { bits: u64::MAX, limit })?;
        let mut result = BigInt::from(1);
        while exponent != 0 {
            if exponent & 1 != 0 {
                result = width_limited_product(&result, &base, limit)?;
            }
            exponent >>= 1;
            if exponent != 0 {
                base = width_limited_product(&base, &base, limit)?;
            }
        }
        Ok(Self::integer(result))
    }
    pub(crate) fn arithmetic_shift(
        self,
        count: Self,
        integer_width: u64,
    ) -> Result<Self, NumericError> {
        if let (Self::Int(value), Self::Int(distance)) = (&self, &count) {
            if *value == 0 || *distance == 0 {
                return Ok(Self::Int(*value).normalize());
            }
            if *distance < 0 {
                let distance = distance.unsigned_abs();
                let shifted = if distance >= 64 {
                    if *value < 0 { -1 } else { 0 }
                } else {
                    *value >> distance
                };
                return Ok(Self::Int(shifted).normalize());
            }
            if *distance < 64 {
                // Widen before shifting: primitive checked_shl checks only
                // the count, not signed magnitude overflow.
                let shifted = i128::from(*value) << *distance;
                if let Ok(shifted) = i64::try_from(shifted) {
                    return Ok(Self::Int(shifted).normalize());
                }
            }
        }
        let value = self.into_integer()?;
        let count = count.into_integer()?;
        if value.is_zero() || count.is_zero() {
            return Ok(Self::integer(value));
        }
        if count.sign() == Sign::Minus {
            let distance = (-count).to_u64();
            let value = match distance.filter(|distance| *distance < value.bits()) {
                Some(distance) => value >> distance,
                None => BigInt::from(if value.sign() == Sign::Minus { -1 } else { 0 }),
            };
            let result = Self::integer(value);
            // Right shifts can still allocate an oversized result when the
            // input predates a smaller dynamically bound integer-width.
            result.check_integer_width(integer_width.max(128))?;
            return Ok(result);
        }
        // GNU ignores integer-width for small machine-sized results. Check
        // the exact prospective magnitude before allocating a shifted bignum.
        let limit = integer_width.max(128);
        let bits = count.to_u64().and_then(|distance| value.bits().checked_add(distance));
        let bits = bits.ok_or(NumericError::IntegerWidth { bits: u64::MAX, limit })?;
        if bits > limit {
            return Err(NumericError::IntegerWidth { bits, limit });
        }
        let distance =
            count.to_u64().ok_or(NumericError::IntegerWidth { bits: u64::MAX, limit })?;
        Ok(Self::integer(value << distance))
    }
    pub(crate) fn checked_div(self, rhs: Self) -> Result<Self, NumericError> {
        if let (Self::Int(numerator), Self::Int(denominator)) = (&self, &rhs) {
            if *denominator == 0 {
                return Err(NumericError::DivisionByZero);
            }
            // The sole overflowing machine quotient is MIN / -1. Promote
            // exactly that case; normal tagged-integer division allocates no BigInt.
            return Ok(match numerator.checked_div(*denominator) {
                Some(value) => Self::Int(value).normalize(),
                None => Self::Big(BigInt::from(1_u8) << 63_usize),
            });
        }
        if matches!(self, Self::Float(_)) || matches!(rhs, Self::Float(_)) {
            return Ok(Self::Float(self.to_float() / rhs.to_float()));
        }
        let denominator = rhs.into_integer()?;
        if denominator.is_zero() {
            return Err(NumericError::DivisionByZero);
        }
        Ok(Self::integer(self.into_integer()? / denominator))
    }
    pub(crate) fn checked_rem(self, rhs: Self, modulo: bool) -> Result<Self, NumericError> {
        if let (Self::Int(numerator), Self::Int(denominator)) = (&self, &rhs) {
            if *denominator == 0 {
                return Err(NumericError::DivisionByZero);
            }
            // MIN % -1 is mathematically zero, although Rust's primitive
            // operation reports the paired division overflow.
            let mut value = numerator.checked_rem(*denominator).unwrap_or(0);
            if modulo && value != 0 && (value < 0) != (*denominator < 0) {
                // Opposite signs and |value| < |denominator| guarantee this
                // addition remains inside the denominator's signed domain.
                value += denominator;
            }
            return Ok(Self::Int(value).normalize());
        }
        if matches!(self, Self::Float(_)) || matches!(rhs, Self::Float(_)) {
            if !modulo {
                return Err(NumericError::ExpectedInteger);
            }
            let denominator = rhs.to_float();
            let mut value = self.to_float() % denominator;
            if if denominator < 0.0 { value > 0.0 } else { value < 0.0 } {
                value += denominator;
            }
            return Ok(Self::Float(value));
        }
        let denominator = rhs.into_integer()?;
        if denominator.is_zero() {
            return Err(NumericError::DivisionByZero);
        }
        let mut value = self.into_integer()? % &denominator;
        if modulo && !value.is_zero() && value.sign() != denominator.sign() {
            value += denominator;
        }
        Ok(Self::integer(value))
    }
    fn binary(
        self,
        rhs: Self,
        small: fn(i64, i64) -> Option<i64>,
        float: fn(f64, f64) -> f64,
        big: fn(BigInt, BigInt) -> BigInt,
    ) -> Self {
        match (self, rhs) {
            (Self::Int(left), Self::Int(right)) => match small(left, right).and_then(Fixnum::new) {
                Some(value) => Self::Int(value.get()),
                None => Self::integer(big(left.into(), right.into())),
            },
            (Self::Float(left), right) => Self::Float(float(left, right.to_float())),
            (left, Self::Float(right)) => Self::Float(float(left.to_float(), right)),
            (Self::Int(left), Self::Big(right)) => Self::integer(big(left.into(), right)),
            (Self::Big(left), Self::Int(right)) => Self::integer(big(left, right.into())),
            (Self::Big(left), Self::Big(right)) => Self::integer(big(left, right)),
        }
    }
}

fn width_limited_product(
    left: &BigInt,
    right: &BigInt,
    limit: u64,
) -> Result<BigInt, NumericError> {
    // For nonzero integers the product needs a+b-1 or a+b magnitude bits.
    // Reject certain overflow before multiplication; an ambiguous product
    // allocates at most one bit beyond the limit and is then checked exactly.
    let lower = left.bits().checked_add(right.bits()).and_then(|bits| bits.checked_sub(1));
    let lower = lower.ok_or(NumericError::IntegerWidth { bits: u64::MAX, limit })?;
    if lower > limit {
        return Err(NumericError::IntegerWidth { bits: lower, limit });
    }
    let result = left * right;
    let bits = result.bits();
    if bits > limit {
        return Err(NumericError::IntegerWidth { bits, limit });
    }
    Ok(result)
}
impl Add for NumberValue {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.binary(rhs, i64::checked_add, |a, b| a + b, |a, b| a + b)
    }
}
impl Sub for NumberValue {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.binary(rhs, i64::checked_sub, |a, b| a - b, |a, b| a - b)
    }
}
impl Mul for NumberValue {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        self.binary(rhs, i64::checked_mul, |a, b| a * b, |a, b| a * b)
    }
}
impl Neg for NumberValue {
    type Output = Self;
    fn neg(self) -> Self {
        match self {
            Self::Int(value) => Self::integer(-BigInt::from(value)),
            Self::Big(value) => Self::integer(-value),
            Self::Float(value) => Self::Float(-value),
        }
    }
}
impl Zero for NumberValue {
    fn zero() -> Self {
        Self::Int(0)
    }
    fn is_zero(&self) -> bool {
        match self {
            Self::Int(x) => *x == 0,
            Self::Big(x) => x.is_zero(),
            Self::Float(x) => *x == 0.0,
        }
    }
}

/// Compare exactly without rounding an integer into an IEEE double first.
fn integer_float(integer: &BigInt, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    if float.is_infinite() {
        return Some(if float.is_sign_positive() { Ordering::Less } else { Ordering::Greater });
    }
    let truncated = BigInt::from_f64(float)?;
    let order = integer.cmp(&truncated);
    if order != Ordering::Equal || float.fract() == 0.0 {
        return Some(order);
    }
    Some(if float.is_sign_positive() { Ordering::Less } else { Ordering::Greater })
}
fn bigint_float(value: &BigInt) -> f64 {
    value.to_f64().unwrap_or_else(|| {
        if value.sign() == Sign::Minus { f64::NEG_INFINITY } else { f64::INFINITY }
    })
}
fn compare_refs(left: NumericRef<'_>, right: NumericRef<'_>) -> Option<Ordering> {
    use NumericRef::{Big, Float, Int};
    match (left, right) {
        (Int(a), Int(b)) => a.partial_cmp(&b),
        (Big(a), Big(b)) => Some(a.cmp(b)),
        (Int(a), Big(b)) => Some(BigInt::from(a).cmp(b)),
        (Big(a), Int(b)) => Some(a.cmp(&BigInt::from(b))),
        (Float(a), Float(b)) => a.partial_cmp(&b),
        (Int(a), Float(b)) => integer_float(&BigInt::from(a), b),
        (Big(a), Float(b)) => integer_float(a, b),
        (Float(a), Int(b)) => integer_float(&BigInt::from(b), a).map(Ordering::reverse),
        (Float(a), Big(b)) => integer_float(b, a).map(Ordering::reverse),
    }
}
impl PartialOrd for NumberValue {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        compare_refs(self.numeric_ref(), rhs.numeric_ref())
    }
}
impl PartialEq for NumberValue {
    fn eq(&self, rhs: &Self) -> bool {
        self.partial_cmp(rhs) == Some(Ordering::Equal)
    }
}
impl PartialEq<i64> for Number<'_> {
    fn eq(&self, rhs: &i64) -> bool {
        compare_refs(self.numeric_ref(), NumericRef::Int(*rhs)) == Some(Ordering::Equal)
    }
}
impl PartialEq<f64> for Number<'_> {
    fn eq(&self, rhs: &f64) -> bool {
        compare_refs(self.numeric_ref(), NumericRef::Float(*rhs)) == Some(Ordering::Equal)
    }
}
impl PartialEq<BigInt> for Number<'_> {
    fn eq(&self, rhs: &BigInt) -> bool {
        compare_refs(self.numeric_ref(), NumericRef::Big(rhs)) == Some(Ordering::Equal)
    }
}
impl IntoObject for NumberValue {
    type Out<'ob> = ObjectType<'ob>;
    fn into_obj<const C: bool>(self, block: &Block<C>) -> Gc<Self::Out<'_>> {
        match self.normalize() {
            Self::Int(value) => value.into(),
            Self::Float(value) => block.add(value),
            Self::Big(value) => block.add(value),
        }
    }
}
impl<const C: bool> Block<C> {
    pub(crate) fn make_integer(&self, value: impl Into<BigInt>) -> Object<'_> {
        self.add(NumberValue::integer(value))
    }
}
impl Context<'_> {
    pub(crate) fn make_integer(&self, value: impl Into<BigInt>) -> Object<'_> {
        self.add(NumberValue::integer(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::gc::RootSet, interpreter::assert_lisp};
    use proptest::prelude::*;

    #[test]
    fn machine_power_boundary_and_huge_exponents_match_gnu() {
        for base in [i64::MIN, MIN_FIXNUM, -3, -2, -1, 0, 1, 2, 3, MAX_FIXNUM, i64::MAX] {
            let mut form = String::from("(let ((integer-width 128)) (list");
            for power in [0, 1, 2, 3, 7, 31, 32, 55, 56, 63, 64, 127, 128, 129] {
                form.push_str(&format!(" (condition-case nil (expt {base} {power}) (error 'caught))"));
            }
            form.push_str("))");
            crate::assert_elprop!("{}", form);
        }
        for base in [-1, 0, 1] {
            for power in [i64::MAX - 1, i64::MAX] {
                let actual = NumberValue::Int(base).checked_pow(NumberValue::Int(power), 128).unwrap();
                let expected = match base { -1 => if power & 1 == 0 { 1 } else { -1 }, 0 => 0, _ => 1 };
                assert_eq!(actual, NumberValue::Int(expected));
            }
        }
    }

    #[test]
    fn machine_shift_boundary_matrix_matches_gnu() {
        let values = [i64::MIN, i64::MIN + 1, MIN_FIXNUM - 1, MIN_FIXNUM,
            -7, -1, 0, 1, 7, MAX_FIXNUM, MAX_FIXNUM + 1, i64::MAX - 1, i64::MAX];
        let counts = [-1000, -128, -64, -63, -56, -1, 0, 1, 7, 54, 55, 56, 63, 64, 100, 128, 1000];
        for left in values {
            let mut form = String::from("(let ((integer-width 128)) (list");
            for count in counts {
                form.push_str(&format!(" (condition-case nil (ash {left} {count}) (error 'caught))"));
            }
            form.push_str("))");
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn machine_shift_extreme_counts_and_owned_values_are_safe() {
        for value in [i64::MIN, i64::MAX, MIN_FIXNUM, MAX_FIXNUM, -1, 0, 1] {
            for count in [i64::MIN, -64, -63, -1, 0, 1, 63, 64, i64::MAX] {
                let actual = NumberValue::Int(value).arithmetic_shift(NumberValue::Int(count), 128);
                if count == i64::MAX && value != 0 {
                    assert!(matches!(actual, Err(NumericError::IntegerWidth { .. })));
                } else {
                    let expected = if count < 0 {
                        if count <= -64 { BigInt::from(if value < 0 { -1 } else { 0 }) }
                        else { BigInt::from(value) >> count.unsigned_abs() }
                    } else if value == 0 { BigInt::from(0) }
                    else { BigInt::from(value) << count as u64 };
                    assert_eq!(actual.unwrap().into_integer().unwrap(), expected);
                }
            }
        }
    }

    #[test]
    fn machine_bitwise_owned_extremes_normalize_results() {
        for left in [i64::MIN, i64::MAX, MIN_FIXNUM, MAX_FIXNUM, -1, 0, 1] {
            assert_eq!(NumberValue::Int(left).bit_not().unwrap().into_integer().unwrap(), !BigInt::from(left));
            for right in [i64::MIN, i64::MAX, MIN_FIXNUM, MAX_FIXNUM, -1, 0, 1] {
                let x = BigInt::from(left);
                let y = BigInt::from(right);
                for (actual, expected) in [
                    (NumberValue::Int(left).bit_and(NumberValue::Int(right)).unwrap(), &x & &y),
                    (NumberValue::Int(left).bit_or(NumberValue::Int(right)).unwrap(), &x | &y),
                    (NumberValue::Int(left).bit_xor(NumberValue::Int(right)).unwrap(), &x ^ &y),
                ] {
                    assert_eq!(matches!(actual, NumberValue::Int(_)), expected.to_i64().and_then(Fixnum::new).is_some());
                    assert_eq!(actual.into_integer().unwrap(), expected);
                }
            }
        }
        assert_eq!(NumberValue::Int(i64::MIN).bit_count().unwrap(), NumberValue::Int(63));
        assert_eq!(NumberValue::Int(i64::MAX).bit_count().unwrap(), NumberValue::Int(63));
        assert_eq!(NumberValue::Int(-1).bit_count().unwrap(), NumberValue::Int(0));
    }

    #[test]
    fn machine_bitwise_boundary_matrix_matches_gnu() {
        let values = [i64::MIN, i64::MIN + 1, MIN_FIXNUM - 1, MIN_FIXNUM,
            -7, -1, 0, 1, 7, MAX_FIXNUM, MAX_FIXNUM + 1, i64::MAX - 1, i64::MAX];
        for left in values {
            let mut form = format!("(list (lognot {left}) (logcount {left})");
            for right in values {
                for operation in ["logand", "logior", "logxor"] {
                    form.push_str(&format!(" ({operation} {left} {right})"));
                }
            }
            form.push(')');
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn machine_divrem_lisp_boundary_matrix_matches_gnu() {
        let values = [i64::MIN, i64::MIN + 1, MIN_FIXNUM - 1, MIN_FIXNUM,
            -7, -1, 0, 1, 7, MAX_FIXNUM, MAX_FIXNUM + 1, i64::MAX - 1, i64::MAX];
        for left in values {
            let mut form = String::from("(list");
            for right in values {
                for operation in ["/", "%", "mod"] {
                    form.push_str(&format!(" (condition-case nil ({operation} {left} {right}) (error 'caught))"));
                }
            }
            form.push(')');
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn machine_division_and_remainder_cover_full_i64_domain() {
        let values = [i64::MIN, i64::MIN + 1, MIN_FIXNUM - 1, MIN_FIXNUM,
            -7, -1, 0, 1, 7, MAX_FIXNUM, MAX_FIXNUM + 1, i64::MAX - 1, i64::MAX];
        for left in values {
            for right in values {
                let x = BigInt::from(left);
                let y = BigInt::from(right);
                if right == 0 {
                    assert_eq!(NumberValue::Int(left).checked_div(NumberValue::Int(right)), Err(NumericError::DivisionByZero));
                    for modulo in [false, true] {
                        assert_eq!(NumberValue::Int(left).checked_rem(NumberValue::Int(right), modulo), Err(NumericError::DivisionByZero));
                    }
                    continue;
                }
                let quotient = NumberValue::Int(left).checked_div(NumberValue::Int(right)).unwrap();
                assert_eq!(quotient.clone().into_integer().unwrap(), &x / &y);
                assert!(matches!(quotient, NumberValue::Int(_)) == (&x / &y).to_i64().and_then(Fixnum::new).is_some());
                for modulo in [false, true] {
                    let mut expected = &x % &y;
                    if modulo && !expected.is_zero() && expected.sign() != y.sign() {
                        expected += &y;
                    }
                    let actual = NumberValue::Int(left).checked_rem(NumberValue::Int(right), modulo).unwrap();
                    assert_eq!(actual.clone().into_integer().unwrap(), expected);
                    assert!(matches!(actual, NumberValue::Int(_)) == expected.to_i64().and_then(Fixnum::new).is_some());
                }
            }
        }
    }

    #[test]
    fn exact_comparison_preserves_integer_information_and_ieee_classes() {
        let double = 9_007_199_254_740_992.0;
        let integer = NumberValue::Int(9_007_199_254_740_993);
        assert!(integer > NumberValue::Float(double));
        assert_ne!(integer, NumberValue::Float(double));
        assert_ne!(
            NumberValue::Float(1.0),
            NumberValue::Float(f64::from_bits(1.0_f64.to_bits() + 1))
        );
        assert!(NumberValue::Int(-1) > NumberValue::Float(-1.5));
        assert!(NumberValue::Int(0) < NumberValue::Float(f64::from_bits(1)));
        assert_eq!(NumberValue::Int(0), NumberValue::Float(-0.0));
        let huge = NumberValue::Big(BigInt::from(1_u8) << 4096_usize);
        assert!(huge < NumberValue::Float(f64::INFINITY));
        assert!(huge > NumberValue::Float(f64::MAX));
        assert_eq!(huge.partial_cmp(&NumberValue::Float(f64::NAN)), None);
        assert_ne!(NumberValue::Float(f64::NAN), NumberValue::Float(f64::NAN));
    }

    #[test]
    fn integer_width_accepts_exactly_the_signed_machine_domain() {
        for value in [i64::MIN, -1, 0, 128, 65_536, i64::MAX] {
            assert_eq!(NumberValue::integer(value).into_integer_width().unwrap(), value as u64);
        }
        for value in [
            BigInt::from(i64::MIN) - 1_u8,
            BigInt::from(i64::MAX) + 1_u8,
            BigInt::from(u64::MAX),
            BigInt::from(1_u8) << 128_usize,
        ] {
            assert_eq!(
                NumberValue::Big(value).into_integer_width(),
                Err(NumericError::IntegerWidthOutOfRange)
            );
        }
        assert_eq!(
            NumberValue::Float(1.0).into_integer_width(),
            Err(NumericError::ExpectedInteger)
        );
    }

    #[test]
    fn powers_are_exact_fallible_and_width_limited_before_large_allocation() {
        for base in -9_i64..=9 {
            for exponent in 0_i64..=31 {
                let actual = NumberValue::integer(base)
                    .checked_pow(NumberValue::integer(exponent), 256)
                    .unwrap();
                assert_eq!(actual.into_integer().unwrap(), BigInt::from(base).pow(exponent as u32));
            }
        }
        // Exercise the ambiguous a+b-1/a+b product boundary with references
        // no larger than 317 bits. A preflight must never reject a fitting power.
        for base in -9_i64..=9 {
            for exponent in 0_u32..=100 {
                let reference = BigInt::from(base).pow(exponent);
                for width in [0_u64, 128, 129, 160, 256, 320] {
                    let actual = NumberValue::integer(base)
                        .checked_pow(NumberValue::integer(exponent), width);
                    if reference.bits() <= width.max(128) {
                        assert_eq!(actual.unwrap().into_integer().unwrap(), reference);
                    } else {
                        assert!(matches!(actual, Err(NumericError::IntegerWidth { .. })));
                    }
                }
            }
        }
        let power = |base, exponent, width| {
            NumberValue::integer(base).checked_pow(NumberValue::integer(exponent), width)
        };
        assert!(power(2_i64, 127_i64, 0).is_ok());
        assert!(power(2, 128, 0).is_err());
        assert!(power(2, 128, 129).is_ok());
        assert!(power(2, 129, 129).is_err());
        assert!(power(3, 81, 129).is_ok());
        assert!(power(3, 82, 129).is_err());
        assert_eq!(power(0, 0, 0).unwrap(), NumberValue::Int(1));
        let huge = (BigInt::from(1) << 128_usize) + 1_u8;
        for (base, expected) in [(0, 0), (1, 1), (-1, -1)] {
            assert_eq!(
                NumberValue::integer(base)
                    .checked_pow(NumberValue::Big(huge.clone()), 0)
                    .unwrap(),
                NumberValue::Int(expected)
            );
        }
        assert!(NumberValue::Int(2).checked_pow(NumberValue::Big(huge), 65_536).is_err());
        assert_eq!(power(2, -1, 0).unwrap(), NumberValue::Float(0.5));
        assert_eq!(power(-2, -3, 0).unwrap(), NumberValue::Float(-0.125));
        assert_eq!(
            NumberValue::Float(2.0).checked_pow(NumberValue::Int(3), 0).unwrap(),
            NumberValue::Float(8.0)
        );
    }

    #[test]
    fn bit_count_matches_infinite_twos_complement() {
        for value in -1024_i64..=1024 {
            let expected = if value < 0 { (!value).count_ones() } else { value.count_ones() };
            assert_eq!(
                NumberValue::integer(value).bit_count().unwrap(),
                NumberValue::Int(i64::from(expected))
            );
        }
        for width in [55_usize, 56, 63, 64, 127, 128, 255, 256, 1024] {
            let bit = BigInt::from(1) << width;
            assert_eq!(NumberValue::Big(bit.clone()).bit_count().unwrap(), NumberValue::Int(1));
            assert_eq!(
                NumberValue::Big(-&bit).bit_count().unwrap(),
                NumberValue::Int(width as i64)
            );
            assert_eq!(
                NumberValue::Big(&bit - 1_u8).bit_count().unwrap(),
                NumberValue::Int(width as i64)
            );
            assert_eq!(NumberValue::Big(-bit - 1_u8).bit_count().unwrap(), NumberValue::Int(1));
        }
        assert_eq!(NumberValue::Float(1.0).bit_count(), Err(NumericError::ExpectedInteger));
    }

    #[test]
    fn extrema_preserve_first_equal_kind_signed_zero_and_nan() {
        for select in [NumberValue::maximum, NumberValue::minimum] {
            assert!(matches!(
                select(NumberValue::Int(1), NumberValue::Float(1.0)),
                NumberValue::Int(1)
            ));
            assert!(matches!(
                select(NumberValue::Float(1.0), NumberValue::Int(1)),
                NumberValue::Float(1.0)
            ));
            for (first, last) in [(f64::NAN, -f64::NAN), (-f64::NAN, f64::NAN)] {
                let NumberValue::Float(value) =
                    select(NumberValue::Float(first), NumberValue::Float(last))
                else {
                    panic!("expected NaN")
                };
                assert_eq!(value.to_bits(), last.to_bits());
            }
            for zero in [-0.0_f64, 0.0] {
                let NumberValue::Float(result) =
                    select(NumberValue::Float(zero), NumberValue::Float(-zero))
                else {
                    panic!("expected float zero")
                };
                assert_eq!(result.to_bits(), zero.to_bits());
            }
            for (left, right) in [
                (NumberValue::Int(1), NumberValue::Float(f64::NAN)),
                (NumberValue::Float(f64::NAN), NumberValue::Int(1)),
            ] {
                let result = select(left, right);
                let result = select(result, NumberValue::Int(2));
                assert!(matches!(result, NumberValue::Float(value) if value.is_nan()));
            }
        }
        let exact = NumberValue::integer(9_007_199_254_740_993_i64);
        let rounded = NumberValue::Float(9_007_199_254_740_992.0);
        assert!(matches!(
            exact.clone().maximum(rounded.clone()),
            NumberValue::Int(9_007_199_254_740_993)
        ));
        assert!(matches!(exact.minimum(rounded), NumberValue::Float(_)));
    }

    #[test]
    fn owned_integer_operations_normalize_at_every_boundary() {
        let values = [
            i64::MIN,
            MIN_FIXNUM - 1,
            MIN_FIXNUM,
            -7,
            -1,
            0,
            1,
            7,
            MAX_FIXNUM,
            MAX_FIXNUM + 1,
            i64::MAX,
        ];
        for left in values {
            let a = NumberValue::integer(left);
            assert_eq!(a.clone().into_integer().unwrap(), BigInt::from(left));
            assert_eq!(matches!(a, NumberValue::Int(_)), Fixnum::new(left).is_some());
            assert_eq!(a.clone().bit_not().unwrap().into_integer().unwrap(), !BigInt::from(left));
            for right in values {
                let b = NumberValue::integer(right);
                let x = BigInt::from(left);
                let y = BigInt::from(right);
                for (actual, expected) in [
                    (a.clone().bit_and(b.clone()).unwrap(), &x & &y),
                    (a.clone().bit_or(b.clone()).unwrap(), &x | &y),
                    (a.clone().bit_xor(b.clone()).unwrap(), &x ^ &y),
                ] {
                    assert_eq!(actual.clone().into_integer().unwrap(), expected);
                    assert_eq!(actual.clone().normalize(), actual);
                }
                if right != 0 {
                    assert_eq!(
                        a.clone().checked_div(b.clone()).unwrap().into_integer().unwrap(),
                        &x / &y
                    );
                    assert_eq!(
                        a.clone().checked_rem(b.clone(), false).unwrap().into_integer().unwrap(),
                        &x % &y
                    );
                    let modulo = a.clone().checked_rem(b, true).unwrap().into_integer().unwrap();
                    assert!(modulo.is_zero() || modulo.sign() == y.sign());
                    assert!(modulo.magnitude() < y.magnitude());
                    assert!((&x - modulo) % &y == BigInt::zero());
                }
            }
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                NumberValue::Float(value).try_truncate(),
                Err(NumericError::NonFiniteInteger)
            );
        }
    }

    #[test]
    fn canonical_construction_is_the_only_numeric_object_boundary() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for value in [
            i64::MIN,
            MIN_FIXNUM - 1,
            MIN_FIXNUM,
            -1,
            0,
            MAX_FIXNUM,
            MAX_FIXNUM + 1,
            i64::MAX,
        ] {
            let object = cx.make_integer(value);
            let number = Number::try_from(object).unwrap();
            assert_eq!(number.val().into_integer().unwrap(), BigInt::from(value));
            assert_eq!(object.is_int(), Fixnum::new(value).is_some());
            let object = cx.add(NumberValue::Int(value));
            assert_eq!(
                Number::try_from(object).unwrap().val().into_integer().unwrap(),
                BigInt::from(value)
            );
        }
        assert!(cx.make_integer(BigInt::from(7)).is_int());
        assert!(NumberValue::Float(f64::NAN).try_truncate().is_err());
        assert!(NumberValue::Float(f64::INFINITY).try_truncate().is_err());
        assert!(matches!(NumberValue::Float(f64::NAN).coerce_integer(), NumberValue::Float(_)));
    }

    #[test]
    fn fallible_operations_never_use_raw_integer_division_or_shift() {
        let zero = NumberValue::Int(0);
        assert_eq!(
            NumberValue::Int(1).checked_div(zero.clone()),
            Err(NumericError::DivisionByZero)
        );
        assert_eq!(
            NumberValue::Int(1).checked_rem(zero.clone(), false),
            Err(NumericError::DivisionByZero)
        );
        assert_eq!(
            NumberValue::Float(1.0).checked_rem(NumberValue::Int(2), false),
            Err(NumericError::ExpectedInteger)
        );
        let huge_count = NumberValue::Big(BigInt::from(1_u8) << 128_usize);
        assert_eq!(zero.arithmetic_shift(huge_count.clone(), 128).unwrap(), NumberValue::Int(0));
        assert!(NumberValue::Int(1).arithmetic_shift(huge_count.clone(), 128).is_err());
        assert_eq!(
            NumberValue::Int(-7).arithmetic_shift(-huge_count, 128).unwrap(),
            NumberValue::Int(-1)
        );
        assert_eq!(
            NumberValue::Int(1).arithmetic_shift(NumberValue::Int(127), 0).unwrap(),
            NumberValue::Big(BigInt::from(1_u8) << 127_usize)
        );
        assert!(NumberValue::Int(1).arithmetic_shift(NumberValue::Int(128), 128).is_err());
        assert_eq!(
            NumberValue::Int(-1)
                .bit_and(NumberValue::Big(BigInt::from(1_u8) << 100_usize))
                .unwrap(),
            NumberValue::Big(BigInt::from(1_u8) << 100_usize)
        );
    }

    #[test]
    fn borrowed_integer_width_decoding_uses_exact_signed_machine_bounds() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for value in [i64::MIN, -1, 0, 128, i64::MAX] {
            let number = Number::try_from(cx.make_integer(value)).unwrap();
            assert_eq!(number.integer_width().unwrap(), value as u64);
        }
        for value in [
            BigInt::from(i64::MIN) - 1_u8,
            BigInt::from(i64::MAX) + 1_u8,
            BigInt::from(1_u8) << 100_000_usize,
        ] {
            let number: Number = cx.add_as(value);
            assert_eq!(number.integer_width(), Err(NumericError::IntegerWidthOutOfRange));
        }
        let float: Number = cx.add_as(1.0);
        assert_eq!(float.integer_width(), Err(NumericError::ExpectedInteger));
    }

    #[test]
    fn right_shift_enforces_result_width_but_zero_count_is_unchanged() {
        let power = BigInt::from(1_u8) << 200_usize;
        for value in [power.clone(), -power.clone(), &power - 1_u8, -(&power - 1_u8)] {
            let number = NumberValue::Big(value.clone());
            assert_eq!(number.clone().arithmetic_shift(NumberValue::Int(0), 128).unwrap(), number);
            let expected = &value >> 72_usize;
            let actual = number.clone().arithmetic_shift(NumberValue::Int(-72), 128);
            if expected.bits() > 128 {
                assert!(actual.is_err());
            } else {
                assert_eq!(actual.unwrap().into_integer().unwrap(), expected);
            }
            let result = number.clone().arithmetic_shift(NumberValue::Int(-73), 128).unwrap();
            assert_eq!(result.into_integer().unwrap(), &value >> 73_usize);
            for distance in 0_usize..203 {
                let expected = &value >> distance;
                for width in [0, 127, 128, 129, 200] {
                    let actual = number
                        .clone()
                        .arithmetic_shift(NumberValue::Int(-(distance as i64)), width);
                    if distance != 0 && expected.bits() > width.max(128) {
                        assert!(actual.is_err());
                    } else {
                        assert_eq!(actual.unwrap().into_integer().unwrap(), expected);
                    }
                }
            }
        }
        assert_lisp(
            "(let ((x (ash 1 200))) (let ((integer-width 128)) (condition-case nil (ash x -1) (error 'caught))))",
            "caught",
        );
        assert_lisp(
            "(let ((x (ash 1 200))) (let ((integer-width 128)) (= (ash x -73) 170141183460469231731687303715884105728)))",
            "t",
        );
    }

    #[test]
    fn primitive_migration_uses_exact_numbers_and_bignum_bit_operations() {
        for (form, expected) in [
            ("(= 1.0 1.0000000000000002)", "nil"),
            ("(= 9007199254740993 9007199254740992.0)", "nil"),
            ("(> 9007199254740993 9007199254740992.0)", "t"),
            ("(< 9007199254740992.0 9007199254740993)", "t"),
            ("(logand 18446744073709551615 255)", "255"),
            ("(logior 18446744073709551616 1)", "18446744073709551617"),
            ("(logxor 18446744073709551617 1)", "18446744073709551616"),
            ("(lognot 18446744073709551616)", "-18446744073709551617"),
            ("(ash 1 64)", "18446744073709551616"),
            ("(ash -7 -1)", "-4"),
            ("(ash -1 -999999999999999999999999999999)", "-1"),
            (
                "(let ((integer-width 128)) (condition-case nil (ash 1 129) (error 'caught)))",
                "caught",
            ),
            ("(bignump (ash 1 64))", "t"),
            ("(fixnump (ash 1 4))", "t"),
        ] {
            assert_lisp(form, expected);
        }
    }

    proptest! {
        #[test]
        fn machine_powers_match_bigint(base in any::<i64>(), power in 0_u32..130, limit in 128_u64..257) {
            let expected = BigInt::from(base).pow(power);
            let actual = NumberValue::Int(base).checked_pow(NumberValue::Int(i64::from(power)), limit);
            if expected.bits() > limit {
                prop_assert!(matches!(actual, Err(NumericError::IntegerWidth { .. })), "expected integer-width rejection");
            } else {
                let actual = actual.unwrap();
                prop_assert_eq!(matches!(actual, NumberValue::Int(_)), expected.to_i64().and_then(Fixnum::new).is_some());
                prop_assert_eq!(actual.into_integer().unwrap(), expected);
            }
        }

        #[test]
        fn machine_shifts_match_bigint(
            value in any::<i64>(), distance in -256_i64..257, limit in 128_u64..257
        ) {
            let actual = NumberValue::Int(value).arithmetic_shift(NumberValue::Int(distance), limit);
            let expected = if distance < 0 { BigInt::from(value) >> distance.unsigned_abs() }
                else { BigInt::from(value) << distance as u64 };
            if value != 0 && distance > 0 && expected.bits() > limit {
                prop_assert!(matches!(actual, Err(NumericError::IntegerWidth { .. })), "expected integer-width rejection");
            } else {
                let actual = actual.unwrap();
                prop_assert_eq!(matches!(actual, NumberValue::Int(_)), expected.to_i64().and_then(Fixnum::new).is_some());
                prop_assert_eq!(actual.into_integer().unwrap(), expected);
            }
        }

        #[test]
        fn machine_bitwise_matches_bigint(left in any::<i64>(), right in any::<i64>()) {
            let x = BigInt::from(left);
            let y = BigInt::from(right);
            for (actual, expected) in [
                (NumberValue::Int(left).bit_and(NumberValue::Int(right)).unwrap(), &x & &y),
                (NumberValue::Int(left).bit_or(NumberValue::Int(right)).unwrap(), &x | &y),
                (NumberValue::Int(left).bit_xor(NumberValue::Int(right)).unwrap(), &x ^ &y),
                (NumberValue::Int(left).bit_not().unwrap(), !&x),
            ] {
                prop_assert_eq!(matches!(actual, NumberValue::Int(_)), expected.to_i64().and_then(Fixnum::new).is_some());
                prop_assert_eq!(actual.into_integer().unwrap(), expected);
            }
            let magnitude = if left < 0 { !x } else { x };
            let expected: u64 = magnitude.magnitude().iter_u64_digits().map(|limb| u64::from(limb.count_ones())).sum();
            let actual = NumberValue::Int(left).bit_count().unwrap();
            prop_assert_eq!(actual.into_integer().unwrap(), BigInt::from(expected));
        }

        #[test]
        fn machine_divrem_matches_bigint(left in any::<i64>(), right in any::<i64>()) {
            if right != 0 {
                let x = BigInt::from(left);
                let y = BigInt::from(right);
                let quotient = NumberValue::Int(left).checked_div(NumberValue::Int(right)).unwrap();
                prop_assert_eq!(quotient.into_integer().unwrap(), &x / &y);
                for modulo in [false, true] {
                    let mut expected = &x % &y;
                    if modulo && !expected.is_zero() && expected.sign() != y.sign() {
                        expected += &y;
                    }
                    let actual = NumberValue::Int(left).checked_rem(NumberValue::Int(right), modulo).unwrap();
                    prop_assert_eq!(actual.into_integer().unwrap(), expected);
                }
            }
        }

        #[test]
        fn make_integer_round_trips_every_i64(value in any::<i64>()) {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let object = cx.make_integer(value);
            let actual = Number::try_from(object).unwrap().val().into_integer().unwrap();
            prop_assert_eq!(actual, BigInt::from(value));
        }
        #[test]
        fn integer_float_order_is_symmetric(integer in any::<i64>(), bits in any::<u64>()) {
            let left = NumberValue::integer(integer);
            let right = NumberValue::Float(f64::from_bits(bits));
            prop_assert_eq!(left.partial_cmp(&right), right.partial_cmp(&left).map(Ordering::reverse));
            prop_assert_eq!(left == right, left.partial_cmp(&right)==Some(Ordering::Equal));
        }
        #[test]
        fn arithmetic_matches_bigint_reference(left in any::<i64>(), right in any::<i64>()) {
            let a = NumberValue::Int(left);
            let b = NumberValue::Int(right);
            prop_assert_eq!((a.clone()+b.clone()).into_integer().unwrap(), BigInt::from(left)+BigInt::from(right));
            prop_assert_eq!((a.clone()-b.clone()).into_integer().unwrap(), BigInt::from(left)-BigInt::from(right));
            prop_assert_eq!((a*b).into_integer().unwrap(), BigInt::from(left)*BigInt::from(right));
        }
    }
}
