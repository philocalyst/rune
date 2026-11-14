//! Utilities for variables and values.
use crate::core::{
    cons::Cons,
    env::{Env, INTERNED_SYMBOLS, sym},
    error::{Type, TypeError},
    gc::{Context, Rt},
    object::{
        HeapDomainValue, IntoObject, List, ListType, NIL, Number, Object, ObjectType, SubrFn,
        Symbol,
    },
};
use anyhow::{Result, anyhow};
use rune_core::macros::list;
use rune_macros::defun;

#[defun]
pub(crate) fn fset<'ob>(symbol: Symbol<'ob>, definition: Object) -> Result<Symbol<'ob>> {
    if definition.is_nil() {
        symbol.unbind_func();
    } else {
        let func = definition.try_into()?;
        let map = INTERNED_SYMBOLS.lock().unwrap();
        map.set_func(symbol, func)?;
    }
    Ok(symbol)
}

#[defun]
pub(crate) fn defalias<'ob>(
    symbol: Symbol<'ob>,
    definition: Object,
    _docstring: Option<&str>,
) -> Result<Symbol<'ob>> {
    fset(symbol, definition)
}

#[defun]
pub(crate) fn set<'ob>(
    place: Symbol,
    newlet: Object<'ob>,
    env: &mut Rt<Env>,
) -> Result<Object<'ob>> {
    env.set_var(place, newlet)?;
    Ok(newlet)
}

#[defun]
pub(crate) fn put<'ob>(
    symbol: Symbol,
    propname: Symbol,
    value: Object<'ob>,
    env: &mut Rt<Env>,
) -> Object<'ob> {
    env.set_prop(symbol, propname, value);
    value
}

#[defun]
pub(crate) fn get<'ob>(
    symbol: Symbol,
    propname: Symbol,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Object<'ob> {
    match env.props.get(symbol) {
        Some(plist) => match plist.iter().find(|x| x.0 == propname) {
            Some(element) => cx.bind(element.1.bind(cx)),
            None => NIL,
        },
        None => NIL,
    }
}

#[defun]
pub(crate) fn local_variable_if_set_p(_sym: Symbol) -> bool {
    // TODO: Implement buffer locals
    false
}

#[defun]
pub(crate) fn default_value<'ob>(
    symbol: Symbol,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    // TODO: Implement buffer locals
    symbol_value(symbol, env, cx).ok_or_else(|| anyhow!("Void variable: {symbol}"))
}

#[defun]
pub(crate) fn symbol_function<'ob>(symbol: Symbol, cx: &'ob Context) -> Object<'ob> {
    match symbol.func(cx) {
        Some(f) => f.into(),
        None => NIL,
    }
}

#[defun]
pub(crate) fn symbol_value<'ob>(
    symbol: Symbol,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Option<Object<'ob>> {
    env.vars.get(symbol).map(|x| x.bind(cx))
}

#[defun]
pub(crate) fn symbol_name(symbol: Symbol<'_>) -> &str {
    symbol.get().name()
}

#[defun]
pub(crate) fn null(obj: Object) -> bool {
    obj.is_nil()
}

#[defun]
pub(crate) fn fboundp(symbol: Symbol) -> bool {
    symbol.has_func()
}

#[defun]
pub(crate) fn fmakunbound(symbol: Symbol) -> Symbol {
    symbol.unbind_func();
    symbol
}

#[defun]
pub(crate) fn boundp(symbol: Symbol, env: &Rt<Env>) -> bool {
    env.vars.get(symbol).is_some()
}

#[defun]
pub(crate) fn makunbound<'ob>(symbol: Symbol<'ob>, env: &mut Rt<Env>) -> Symbol<'ob> {
    env.vars.remove(symbol);
    symbol
}

#[defun]
pub(crate) fn default_boundp(symbol: Symbol, env: &Rt<Env>) -> bool {
    env.vars.get(symbol).is_some()
}

#[defun]
pub(crate) fn listp(object: Object) -> bool {
    matches!(object.untag(), ObjectType::NIL | ObjectType::Cons(_))
}

#[defun]
pub(crate) fn nlistp(object: Object) -> bool {
    !listp(object)
}

#[defun]
pub(crate) fn symbolp(object: Object) -> bool {
    object.is_symbol()
}

#[defun]
pub(crate) fn functionp(object: Object, cx: &Context) -> Result<bool> {
    let symbolic = object.is_symbol();
    let object = resolve_function_chain(object, cx)?;
    // An autoload is callable through its symbol, not as a raw cons. Its fifth
    // field marks macro/keymap autoloads, which are not functions (GNU FUNCTIONP).
    if symbolic
        && let ObjectType::Cons(cons) = object.untag()
        && cons.car() == sym::AUTOLOAD
    {
        let mut tail = object;
        for _ in 0..4 {
            match tail.untag() {
                ObjectType::Cons(cons) => tail = cons.cdr(),
                _ => break,
            }
        }
        return Ok(!matches!(tail.untag(), ObjectType::Cons(cons) if !cons.car().is_nil()));
    }
    Ok(match object.untag() {
        ObjectType::ByteFn(_) | ObjectType::SubrFn(_) => true,
        ObjectType::Cons(cons) => cons.car() == sym::CLOSURE || cons.car() == sym::LAMBDA,
        _ => false,
    })
}

#[defun]
pub(crate) fn subrp(object: Object) -> bool {
    object.is_subr_fn()
}

#[defun]
pub(crate) fn stringp(object: Object) -> bool {
    matches!(object.untag(), ObjectType::String(_) | ObjectType::ByteString(_))
}

#[defun]
pub(crate) fn numberp(object: Object) -> bool {
    matches!(
        object.untag(),
        ObjectType::Int(_) | ObjectType::BigInt(_) | ObjectType::Float(_)
    )
}

#[defun]
pub(crate) fn markerp(_: Object) -> bool {
    // TODO: implement
    false
}

#[defun]
pub(crate) fn vectorp(object: Object) -> bool {
    object.is_vec()
}

#[defun]
pub(crate) fn recordp(object: Object) -> bool {
    object.is_record()
}

#[defun]
pub(crate) fn consp(object: Object) -> bool {
    object.is_cons()
}

#[defun]
pub(crate) fn keywordp(object: Object) -> bool {
    match object.untag() {
        ObjectType::Symbol(s) => s.interned() && s.name().starts_with(':'),
        _ => false,
    }
}

#[defun]
pub(crate) fn integerp(object: Object) -> bool {
    matches!(object.untag(), ObjectType::Int(_) | ObjectType::BigInt(_))
}

#[defun]
fn natnump(object: Object) -> bool {
    match object.untag() {
        ObjectType::Int(value) => value >= 0,
        ObjectType::BigInt(value) => value.sign() != num_bigint::Sign::Minus,
        _ => false,
    }
}

#[defun]
pub(crate) fn floatp(object: Object) -> bool {
    object.is_float()
}

#[defun]
pub(crate) fn atom(object: Object) -> bool {
    object.is_not_cons()
}

#[defun]
fn byte_code_function_p(object: Object) -> bool {
    object.is_byte_fn()
}

#[defun]
fn subr_native_elisp_p(_: Object) -> bool {
    false
}

#[defun]
fn bufferp(object: Object) -> bool {
    // Killed buffers remain buffer objects; liveness is a separate predicate.
    matches!(object.untag(), ObjectType::Buffer(_))
}

#[defun]
pub(crate) fn multibyte_string_p(object: Object) -> bool {
    // TODO: handle multi-line strings
    matches!(object.untag(), ObjectType::String(_))
}

#[defun]
fn string_to_number<'ob>(string: &str, base: Option<i64>, cx: &'ob Context) -> Result<Number<'ob>> {
    let base = base.unwrap_or(10);
    // GNU's string-to-number allows 2..=16, unlike the reader's #Nr syntax.
    // Validate before narrowing: Rust's parser panics on some invalid bases,
    // and large Lisp integers must not wrap into an apparently valid radix.
    if !(2..=16).contains(&base) {
        return Err(anyhow!("base {base} is out of range; expected 2 through 16"));
    }
    let base = base as u32;
    // Only space and tab are skipped, not Rust's wider Unicode whitespace set.
    let string = string.trim_start_matches([' ', '\t']);
    let bytes = string.as_bytes();
    let sign = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut end = sign;
    while bytes.get(end).is_some_and(|byte| char::from(*byte).is_digit(base)) {
        end += 1;
    }
    let integer_end = end;
    let leading_digits = end > sign;
    if base == 10 {
        let mut fraction = false;
        if bytes.get(end) == Some(&b'.') {
            end += 1;
            let fraction_start = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            fraction = end > fraction_start;
        }
        let mut exponent = false;
        let mut special = None;
        if matches!(bytes.get(end), Some(b'e' | b'E')) {
            let exponent_start = end;
            end += 1;
            if matches!(bytes.get(end), Some(b'+' | b'-')) {
                end += 1;
            }
            let exponent_digits = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            exponent = end > exponent_digits;
            if !exponent && bytes.get(exponent_digits - 1) == Some(&b'+') {
                let tail = &string[exponent_digits..];
                special = if tail.starts_with("INF") {
                    Some(f64::INFINITY)
                } else if tail.starts_with("NaN") {
                    Some(f64::NAN)
                } else {
                    None
                };
                exponent = special.is_some();
            }
            if !exponent {
                end = exponent_start;
            }
        }
        if fraction || (leading_digits && exponent) {
            let value = match special {
                Some(value) => {
                    if string.starts_with('-') {
                        -value
                    } else {
                        value
                    }
                }
                None => string[..end].parse::<f64>()?,
            };
            return Ok(cx.add_as(value));
        }
    }
    if !leading_digits {
        return Ok(0.into());
    }
    let integer = &string[..integer_end];
    if let Ok(value) = i64::from_str_radix(integer, base) {
        // i64 is wider than the tagged fixnum; normalize before boxing.
        if (crate::arith::MIN_FIXNUM..=crate::arith::MAX_FIXNUM).contains(&value) {
            return Ok(value.into());
        }
    }
    let value = num_bigint::BigInt::parse_bytes(integer.as_bytes(), base)
        .ok_or_else(|| anyhow!("invalid integer prefix"))?;
    Ok(cx.add_as(value))
}

#[defun]
pub(crate) fn defvar<'ob>(
    symbol: Symbol,
    initvalue: Option<Object<'ob>>,
    _docstring: Option<&str>,
    env: &mut Rt<Env>,
) -> Result<Object<'ob>> {
    let value = initvalue.unwrap_or_default();
    set(symbol, value, env)
}

#[defun]
pub(crate) fn make_variable_buffer_local(variable: Symbol) -> Symbol {
    // TODO: Implement
    variable
}

#[defun]
fn subr_arity<'ob>(subr: &SubrFn, cx: &'ob Context) -> Object<'ob> {
    let min = subr.args.required as usize;
    let max: Object = {
        if subr.args.rest {
            sym::MANY.into()
        } else {
            (min + subr.args.optional as usize).into()
        }
    };
    Cons::new(min, max, cx).into()
}

// The numeric policy is a Lisp variable, not an arbitrary host allocation cap.
defvar!(INTEGER_WIDTH, 65_536);
// Bundled subr.el's fixnump/bignump/lsh need the actual storage bounds, not
// GNU's future low-tag encoding bounds.
defvar!(MOST_POSITIVE_FIXNUM, crate::core::object::MAX_FIXNUM);
defvar!(MOST_NEGATIVE_FIXNUM, crate::core::object::MIN_FIXNUM);

pub(crate) fn integer_width(env: &Rt<Env>, cx: &Context) -> Result<u64> {
    match env.vars.get(sym::INTEGER_WIDTH) {
        Some(value) => Ok(Number::try_from(value.bind(cx))?.val().into_integer_width()?),
        None => Ok(65_536),
    }
}

#[defun]
fn ash(
    value: Number,
    count: Number,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<crate::core::object::NumberValue> {
    let limit = integer_width(env, cx)?;
    Ok(value.val().arithmetic_shift(count.val(), limit)?)
}

#[defun]
pub(crate) fn aset<'ob>(
    array: Object<'ob>,
    idx: usize,
    newlet: Object<'ob>,
) -> Result<Object<'ob>> {
    match array.untag() {
        ObjectType::Vec(vec) => {
            let vec = vec.try_mut()?;
            if idx < vec.len() {
                vec[idx].set(newlet);
                Ok(newlet)
            } else {
                let len = vec.len();
                Err(anyhow!("index {idx} is out of bounds. Length was {len}"))
            }
        }
        ObjectType::Record(vec) => {
            let vec = vec.try_mut()?;
            if idx < vec.len() {
                vec[idx].set(newlet);
                Ok(newlet)
            } else {
                let len = vec.len();
                Err(anyhow!("index {idx} is out of bounds. Length was {len}"))
            }
        }
        ObjectType::CharTable(table) => {
            table.set(idx, newlet)?;
            Ok(newlet)
        }
        x => Err(TypeError::new(Type::Sequence, x).into()),
    }
}

#[defun]
pub(crate) fn aref<'ob>(array: Object<'ob>, idx: usize, cx: &'ob Context) -> Result<Object<'ob>> {
    match array.untag() {
        ObjectType::Vec(vec) => match vec.get(idx) {
            Some(x) => Ok(x.get()),
            None => {
                let len = vec.len();
                Err(anyhow!("index {idx} is out of bounds. Length was {len}"))
            }
        },
        ObjectType::Record(vec) => match vec.get(idx) {
            Some(x) => Ok(x.get()),
            None => {
                let len = vec.len();
                Err(anyhow!("index {idx} is out of bounds. Length was {len}"))
            }
        },
        ObjectType::String(string) => match string.chars().nth(idx) {
            Some(x) => Ok((i64::from(x as u32)).into()),
            None => {
                let len = string.len();
                Err(anyhow!("index {idx} is out of bounds. Length was {len}"))
            }
        },
        ObjectType::ByteString(string) => match string.get(idx) {
            Some(x) => Ok((i64::from(*x)).into()),
            None => {
                let len = string.len();
                Err(anyhow!("index {idx} is out of bounds. Length was {len}"))
            }
        },
        ObjectType::ByteFn(fun) => match fun.index(idx, cx) {
            Some(x) => Ok(x),
            None => Err(anyhow!("index {idx} is out of bounds")),
        },
        ObjectType::CharTable(chartable) => Ok(chartable.get(idx)),
        x => Err(TypeError::new(Type::Sequence, x).into()),
    }
}

#[defun]
fn type_of(object: Object) -> Object {
    match object.untag() {
        ObjectType::Int(_) | ObjectType::BigInt(_) => sym::INTEGER.into(),
        ObjectType::Float(_) => sym::FLOAT.into(),
        ObjectType::Symbol(_) => sym::SYMBOL.into(),
        ObjectType::Cons(_) => sym::CONS.into(),
        ObjectType::Vec(_) => sym::VECTOR.into(),
        ObjectType::Record(x) => x.first().expect("record was missing type").get(),
        ObjectType::ByteFn(_) => sym::COMPILED_FUNCTION.into(),
        ObjectType::HashTable(_) => sym::HASH_TABLE.into(),
        ObjectType::String(_) | ObjectType::ByteString(_) => sym::STRING.into(),
        ObjectType::SubrFn(_) => sym::SUBR.into(),
        ObjectType::Buffer(_) => sym::BUFFER.into(),
        ObjectType::CharTable(_) => sym::CHAR_TABLE.into(),
        ObjectType::ChannelSender(_) => sym::CHANNEL_SENDER.into(),
        ObjectType::ChannelReceiver(_) => sym::CHANNEL_RECEIVER.into(),
    }
}

/// Resolve aliases without recursive calls or an allocation per alias.
/// GNU's indirect-function detects cycles; a malformed function chain must
/// cross the Lisp error boundary rather than overflow the Rust stack.
fn resolve_function_chain<'ob>(object: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    fn next<'ob>(object: Object<'ob>, cx: &'ob Context) -> Option<Object<'ob>> {
        match object.untag() {
            ObjectType::Symbol(symbol) if !object.is_nil() => {
                Some(symbol.func(cx).map_or(NIL, Into::into))
            }
            _ => None,
        }
    }

    let mut slow = object;
    let mut fast = object;
    loop {
        let Some(next_fast) = next(fast, cx) else { return Ok(fast) };
        fast = next_fast;
        let Some(next_fast) = next(fast, cx) else { return Ok(fast) };
        fast = next_fast;
        // If the fast cursor advanced twice, the slow cursor can advance once.
        slow = next(slow, cx).ok_or_else(|| anyhow!("function chain changed during lookup"))?;
        if fast.ptr_eq(slow) && fast.is_symbol() && !fast.is_nil() {
            return Err(anyhow!("cyclic function indirection: {object}"));
        }
    }
}

#[defun]
pub(crate) fn indirect_function<'ob>(object: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    resolve_function_chain(object, cx)
}

#[defun]
pub(crate) fn provide<'ob>(
    feature: Symbol<'ob>,
    _subfeatures: Option<&Cons>,
    env: &mut Rt<Env>,
    cx: &Context,
) -> Symbol<'ob> {
    env.provide_feature(feature, cx);
    feature
}

#[defun]
pub(crate) fn car(list: List) -> Object {
    match list.untag() {
        ListType::Cons(cons) => cons.car(),
        ListType::Nil => NIL,
    }
}

#[defun]
pub(crate) fn cdr(list: List) -> Object {
    match list.untag() {
        ListType::Cons(cons) => cons.cdr(),
        ListType::Nil => NIL,
    }
}

#[defun]
pub(crate) fn car_safe(object: Object) -> Object {
    match object.untag() {
        ObjectType::Cons(cons) => cons.car(),
        _ => NIL,
    }
}

#[defun]
pub(crate) fn cdr_safe(object: Object) -> Object {
    match object.untag() {
        ObjectType::Cons(cons) => cons.cdr(),
        _ => NIL,
    }
}

#[defun]
pub(crate) fn setcar<'ob>(cell: &Cons, newcar: Object<'ob>) -> Result<Object<'ob>> {
    cell.set_car(newcar)?;
    Ok(newcar)
}

#[defun]
pub(crate) fn setcdr<'ob>(cell: &Cons, newcdr: Object<'ob>) -> Result<Object<'ob>> {
    cell.set_cdr(newcdr)?;
    Ok(newcdr)
}

#[defun]
pub(crate) fn cons<'ob>(car: Object, cdr: Object, cx: &'ob Context) -> Object<'ob> {
    Cons::new(car, cdr, cx).into()
}

// Symbol with position
#[defun]
fn bare_symbol(sym: Symbol) -> Symbol {
    // TODO: implement
    sym
}

#[defun]
fn symbol_with_pos_p(_sym: Object) -> bool {
    // TODO: implement
    false
}

#[derive(Debug)]
pub(crate) struct LispError {
    // Reuse the domain-owned slot store rather than hiding an unrooted static
    // Cons in an anyhow error. The owned rendering remains safe after teardown
    // and when an error is transferred to a different heap domain.
    message: HeapDomainValue,
    rendered: String,
}

// Host diagnostic comparison, not Lisp identity or condition matching. The
// owned rendering intentionally compares safely even after heap retirement.
impl PartialEq for LispError {
    fn eq(&self, other: &Self) -> bool {
        self.rendered == other.rendered
    }
}

impl std::error::Error for LispError {}

impl std::fmt::Display for LispError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Error: {}", self.rendered)
    }
}

defsym!(WRONG_NUMBER_OF_ARGUMENTS);
impl LispError {
    fn new(message: &Cons, cx: &Context) -> Self {
        Self {
            // SAFETY: The sole caller builds this message list in cx below;
            // its inputs are the evaluator's current-heap argument values.
            message: unsafe { HeapDomainValue::new(cx.add(message), cx) },
            rendered: message.to_string(),
        }
    }

    pub(crate) fn bind<'ob>(&self, cx: &'ob Context) -> &'ob Cons {
        if let Ok(message) = self.message.bind(cx) {
            return message.cons().expect("error message must be a cons");
        }
        // Foreign/retired heaps cannot safely recover original Lisp identity.
        // Preserve the owned diagnostic without transmitting borrowed pointers.
        Cons::new(sym::ERROR, self.rendered.as_str(), cx)
    }

    pub(crate) fn arg_cnt<'ob, T>(
        func: impl IntoObject<Out<'ob> = T>,
        expected: u16,
        actual: u16,
        cx: &'ob Context,
    ) -> Self {
        let func = func.into_obj(cx);
        let list = list![sym::WRONG_NUMBER_OF_ARGUMENTS, func, expected, actual; cx];
        Self::new(list.try_into().unwrap(), cx)
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn bootstrap_numeric_bounds_and_natural_integer_predicate() {
        use crate::core::{
            env::{Env, sym},
            gc::{Context, RootSet},
        };
        use rune_core::macros::{rebind, root};
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, new(Env), cx);
        // Unlike assert_lisp's deliberately empty environment, exercise the
        // generated variable initializers used by production startup.
        crate::core::env::init_variables(cx, env);
        for (source, expected) in [
            ("most-positive-fixnum", "36028797018963967"),
            ("most-negative-fixnum", "-36028797018963968"),
            ("(= (+ most-negative-fixnum most-positive-fixnum) -1)", "t"),
            ("(fixnump most-positive-fixnum)", "t"),
            ("(bignump (1+ most-positive-fixnum))", "t"),
            ("(natnump 0)", "t"),
            ("(natnump -1)", "nil"),
            ("(natnump 18446744073709551616)", "t"),
            ("(natnump -18446744073709551616)", "nil"),
            ("(natnump 0.0)", "nil"),
            ("(natnump nil)", "nil"),
        ] {
            let actual = {
                let form = crate::reader::read(source, cx).unwrap().0;
                root!(form, cx);
                rebind!(crate::interpreter::eval(form, None, env, cx).unwrap(), cx)
            };
            root!(actual, cx);
            let expected = crate::reader::read(expected, cx).unwrap().0;
            assert_eq!(actual.bind(cx), expected, "{source}");
        }
    }

    use super::*;
    use crate::interpreter::assert_lisp;

    #[test]
    fn test_ash() {
        for (value, count, expected) in [
            (4, 1, 8),
            (4, -1, 2),
            (-8, -1, -4),
            (-3, -1, -2),
            (256, -8, 1),
            (-8, 1, -16),
            (i64::MIN, -1, i64::MIN / 2),
            (i64::MIN, 0, i64::MIN),
            (1, -63, 0),
            (-3, -63, -1),
            (1, -64, 0),
            (-3, -64, -1),
            (1, i64::MIN, 0),
            (-3, i64::MIN, -1),
            (0, i64::MIN, 0),
            (0, i64::MAX, 0),
        ] {
            assert_eq!(
                crate::core::object::NumberValue::Int(value)
                    .arithmetic_shift(crate::core::object::NumberValue::Int(count), 65_536)
                    .unwrap(),
                crate::core::object::NumberValue::integer(expected),
                "ash({value}, {count})"
            );
        }
        assert_lisp("(ash -3 -1)", "-2");
        assert_lisp("(ash -3 -1000)", "-1");
        assert_lisp("(ash 1 -1000)", "0");
    }

    #[test]
    fn m0_aref_indexes_multibyte_strings_by_character() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        let string = cx.add("a\u{e9}\u{1f600}z");
        for (idx, expected) in [(0, 97_i64), (1, 0xe9), (2, 0x1f600), (3, 122)] {
            assert_eq!(aref(string, idx, &cx).unwrap(), cx.add(expected));
        }
        for idx in [4, 5, usize::MAX] {
            assert_eq!(
                aref(string, idx, &cx).unwrap_err().to_string(),
                format!("index {idx} is out of bounds. Length was 4")
            );
        }
    }

    #[test]
    fn m0_aref_preserves_raw_bytes_and_rejects_empty_string_indices() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        // These bytes deliberately do not form UTF-8 and must not be decoded.
        let bytes = cx.add(vec![0_u8, 0x80, 0xff]);
        for (idx, expected) in [(0, 0_i64), (1, 128), (2, 255)] {
            assert_eq!(aref(bytes, idx, &cx).unwrap(), cx.add(expected));
        }
        assert_eq!(
            aref(bytes, 3, &cx).unwrap_err().to_string(),
            "index 3 is out of bounds. Length was 3"
        );
        for empty in [cx.add(""), cx.add(Vec::<u8>::new())] {
            for idx in [0, usize::MAX] {
                assert_eq!(
                    aref(empty, idx, &cx).unwrap_err().to_string(),
                    format!("index {idx} is out of bounds. Length was 0")
                );
            }
        }
    }

    #[test]
    fn m0_string_to_number_rejects_invalid_radices_without_panicking() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for base in [i64::MIN, -1, 0, 1, 17, 36, 37, i64::MAX, (1_i64 << 32) + 10] {
            for input in ["", "10", "1.5"] {
                assert!(string_to_number(input, Some(base), &cx).is_err(), "base {base}");
            }
        }
    }

    #[test]
    fn m0_string_to_number_preserves_valid_radices() {
        assert_lisp("(string-to-number \"101\" 2)", "5");
        assert_lisp("(string-to-number \"f\" 16)", "15");
        assert_lisp("(string-to-number \"-ff\" 16)", "-255");
        assert_lisp("(string-to-number \"42\")", "42");
        assert_lisp("(string-to-number \"1.5\" 10)", "1.5");
    }

    #[test]
    fn m0_string_to_number_reads_numeric_prefixes() {
        for (source, expected) in [
            (r#"(string-to-number "123suffix")"#, "123"),
            (r#"(string-to-number "  \t-12tail")"#, "-12"),
            (r#"(string-to-number "1.5tail")"#, "1.5"),
            (r#"(string-to-number ".5e2tail")"#, "50.0"),
            (r#"(string-to-number "1e+2tail")"#, "100.0"),
            (r#"(string-to-number "1e+tail")"#, "1"),
            (r#"(string-to-number "1.5e+tail")"#, "1.5"),
            (r#"(string-to-number "1.tail")"#, "1"),
            (r#"(string-to-number "ff.tail" 16)"#, "255"),
            (r#"(string-to-number "1.5" 16)"#, "1"),
            (r#"(string-to-number "1e2" 16)"#, "482"),
            (r#"(string-to-number "102" 2)"#, "2"),
            (r#"(string-to-number "9223372036854775808tail")"#, "9223372036854775808"),
            (r#"(string-to-number "-9223372036854775809tail")"#, "-9223372036854775809"),
            (r#"(string-to-number "fffffffffffffffftail" 16)"#, "18446744073709551615"),
            (r#"(string-to-number "inf")"#, "0"),
            (r#"(string-to-number "NaN")"#, "0"),
            (r#"(string-to-number "\n12")"#, "0"),
            (r#"(string-to-number "\u00a012")"#, "0"),
            (r#"(string-to-number "+")"#, "0"),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn m0_string_to_number_preserves_special_float_class_and_sign() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for (source, nan, negative) in [
            ("1.0e+INFtail", false, false),
            ("-1.0e+INFtail", false, true),
            ("0.0e+NaNtail", true, false),
            ("-0.0e+NaNtail", true, true),
            ("-0.0tail", false, true),
        ] {
            let number = string_to_number(source, None, &cx).unwrap();
            let crate::core::object::NumberType::Float(value) = number.untag() else {
                panic!("expected float for {source}");
            };
            assert_eq!(value.is_nan(), nan, "{source}");
            assert_eq!(value.is_sign_negative(), negative, "{source}");
        }
    }

    #[test]
    fn m0_numeric_predicates_include_bignums() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for value in [
            num_bigint::BigInt::from(i64::MAX) + 1_u8,
            num_bigint::BigInt::from(i64::MIN) - 1_u8,
        ] {
            let object = cx.add(value);
            assert!(numberp(object));
            assert!(integerp(object));
            assert!(!floatp(object));
        }
        let float = cx.add(1.5_f64);
        assert!(numberp(float));
        assert!(!integerp(float));
        for nonnumber in [NIL, cx.add("123")] {
            assert!(!numberp(nonnumber));
            assert!(!integerp(nonnumber));
        }
        // The Lisp test helper creates its own context; release this one first.
        drop(cx);
        // Exercise the Lisp primitive dispatch paths as well as direct calls.
        assert_lisp("(numberp 9223372036854775808)", "t");
        assert_lisp("(integerp -9223372036854775809)", "t");
    }

    #[test]
    fn m0_type_of_numeric_values_uses_basic_type_symbols() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for integer in [
            cx.add(1_i64),
            cx.add(num_bigint::BigInt::from(i64::MAX) + 1_u8),
            cx.add(num_bigint::BigInt::from(i64::MIN) - 1_u8),
        ] {
            assert_eq!(type_of(integer), cx.add(sym::INTEGER));
        }
        for float in [0.0, -1.5, f64::INFINITY, f64::NAN] {
            assert_eq!(type_of(cx.add(float)), cx.add(sym::FLOAT));
        }
        drop(cx);
        assert_lisp("(type-of 9223372036854775808)", "integer");
        assert_lisp("(type-of -9223372036854775809)", "integer");
        assert_lisp("(type-of 1.5)", "float");
    }

    #[test]
    fn m0_stringp_accepts_multibyte_and_unibyte_strings() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for string in [cx.add(""), cx.add("a\u{e9}\u{1f600}")] {
            assert!(stringp(string));
            assert!(multibyte_string_p(string));
        }
        for bytes in [cx.add(Vec::<u8>::new()), cx.add(vec![0_u8, 0x80, 0xff])] {
            assert!(stringp(bytes));
            assert!(!multibyte_string_p(bytes));
        }
        for nonstring in [NIL, cx.add(1_i64), cx.add(vec![cx.add(1_i64)])] {
            assert!(!stringp(nonstring));
        }
        drop(cx);
        assert_lisp("(stringp (unibyte-string 255))", "t");
        assert_lisp("(stringp (unibyte-string))", "t");
        assert_lisp("(multibyte-string-p (unibyte-string 255))", "nil");
    }

    #[test]
    fn m0_function_predicate_resolves_alias_targets() {
        for (source, expected) in [
            ("(progn (fset 'm0-data-alias 'identity) (functionp 'm0-data-alias))", "t"),
            (
                "(progn (fset 'm0-data-alias 'm0-data-unbound) (functionp 'm0-data-alias))",
                "nil",
            ),
            (
                "(progn (fset 'm0-data-macro '(macro lambda nil)) (functionp 'm0-data-macro))",
                "nil",
            ),
            (
                "(progn (fset 'm0-data-invalid '(not-a-function)) (functionp 'm0-data-invalid))",
                "nil",
            ),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn m0_function_predicate_distinguishes_autoload_types_and_raw_lists() {
        for (source, expected) in [
            (
                "(progn (fset 'm0-data-autoload '(autoload \"unused\" nil nil nil)) (functionp 'm0-data-autoload))",
                "t",
            ),
            (
                "(progn (fset 'm0-data-autoload '(autoload \"unused\" nil nil macro)) (functionp 'm0-data-autoload))",
                "nil",
            ),
            (
                "(progn (fset 'm0-data-autoload '(autoload \"unused\" nil nil keymap)) (functionp 'm0-data-autoload))",
                "nil",
            ),
            (
                "(progn (fset 'm0-data-autoload '(autoload \"unused\")) (functionp 'm0-data-autoload))",
                "t",
            ),
            (
                "(progn (fset 'm0-data-autoload '(autoload \"unused\" nil nil nil)) (fset 'm0-data-autoload-alias 'm0-data-autoload) (functionp 'm0-data-autoload-alias))",
                "t",
            ),
            ("(functionp '(autoload \"unused\" nil nil nil))", "nil"),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn m0_indirect_function_rejects_cycles_without_recursion() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        let a = crate::core::env::intern("m0-data-cycle-a", &cx);
        let b = crate::core::env::intern("m0-data-cycle-b", &cx);
        fset(a, cx.add(b)).unwrap();
        fset(b, cx.add(a)).unwrap();
        assert!(indirect_function(cx.add(a), &cx).unwrap_err().to_string().contains("cyclic"));
        assert!(functionp(cx.add(a), &cx).is_err());
        fmakunbound(a);
        fmakunbound(b);
        fset(a, cx.add(a)).unwrap();
        assert!(indirect_function(cx.add(a), &cx).is_err());
        fmakunbound(a);
        assert_eq!(indirect_function(cx.add(a), &cx).unwrap(), NIL);
        assert_eq!(indirect_function(cx.add(42_i64), &cx).unwrap(), cx.add(42_i64));
    }

    #[test]
    fn m0_keywordp_requires_an_interned_colon_symbol() {
        let roots = crate::core::gc::RootSet::default();
        let cx = Context::new(&roots);
        for name in [":", ":m0-keyword"] {
            let interned = crate::core::env::intern(name, &cx);
            let uninterned = Symbol::new_uninterned(name, &cx);
            assert!(keywordp(cx.add(interned)));
            assert!(!keywordp(cx.add(uninterned)));
        }
        for nonkeyword in [
            NIL,
            cx.add(1_i64),
            cx.add(":m0-keyword"),
            cx.add(crate::core::env::intern("m0-not-keyword", &cx)),
        ] {
            assert!(!keywordp(nonkeyword));
        }
        drop(cx);
        assert_lisp("(keywordp :m0-keyword)", "t");
        assert_lisp("(keywordp (make-symbol \":m0-keyword\"))", "nil");
        assert_lisp("(keywordp '#::m0-keyword)", "nil");
    }

    #[test]
    fn m0_bufferp_recognizes_live_and_killed_buffer_objects() {
        assert_lisp("(bufferp (get-buffer-create \"m0-bufferp-live\"))", "t");
        assert_lisp(
            "(let ((buffer (get-buffer-create \"m0-bufferp-killed\"))) (kill-buffer buffer) (bufferp buffer))",
            "t",
        );
        assert_lisp("(bufferp \"m0-bufferp-live\")", "nil");
        assert_lisp("(bufferp nil)", "nil");
        assert_lisp("(bufferp 1)", "nil");
    }

    #[test]
    fn test_functionp() {
        assert_lisp("(functionp '(lambda nil))", "t");
    }
}

defsym!(MANY);
defsym!(INTEGER);
defsym!(SYMBOL);
defsym!(COMPILED_FUNCTION);
defsym!(HASH_TABLE);
defsym!(BUFFER);
defsym!(SUBR);
defsym!(CHAR_TABLE);
defsym!(BIG_INT);
defsym!(CHANNEL_SENDER);
defsym!(CHANNEL_RECEIVER);
defsym!(CHANNEL_CLOSED);
defsym!(CHANNEL_FULL);
defsym!(CHANNEL_EMPTY);
defsym!(CHANNEL_TIMEOUT);
defsym!(THREAD_ERRORED);

#[cfg(test)]
mod error_ownership_tests {
    use super::*;
    use crate::core::gc::RootSet;

    #[test]
    fn owned_error_roots_structured_message_across_moving_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let error = LispError::arg_cnt(sym::CAR, 1, 2, cx);
        for _ in 0..10 {
            cx.garbage_collect(true);
            let message = error.bind(cx);
            assert_eq!(message.car(), sym::WRONG_NUMBER_OF_ARGUMENTS);
            assert_eq!(message.cadr().unwrap(), sym::CAR);
            assert!(error.to_string().contains("wrong-number-of-arguments"));
        }
    }

    #[test]
    fn foreign_error_binding_is_owned_during_owner_collection() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let error = LispError::arg_cnt(sym::CAR, 1, 2, cx);
        let thread = std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            for _ in 0..30 {
                let message = error.bind(&cx);
                assert_eq!(message.car(), sym::ERROR);
                let diagnostic = message.cdr();
                assert!(matches!(diagnostic.untag(), ObjectType::String(_)));
                assert!(diagnostic.to_string().contains("wrong-number-of-arguments"));
            }
        });
        for _ in 0..30 {
            cx.garbage_collect(true);
        }
        thread.join().unwrap();
    }

    #[test]
    fn retired_error_transfers_owned_diagnostic_not_heap_pointers() {
        let error = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            LispError::arg_cnt(sym::CAR, 1, 2, &cx)
        };
        std::thread::spawn(move || {
            let rendered = error.to_string();
            assert!(rendered.contains("wrong-number-of-arguments"));
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let message = error.bind(&cx);
            assert_eq!(message.car(), sym::ERROR);
            assert!(message.cdr().to_string().contains("wrong-number-of-arguments"));
        })
        .join()
        .unwrap();
    }
}
