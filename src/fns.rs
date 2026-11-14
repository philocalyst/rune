//! General purpose lisp functions
use crate::{
    core::{
        cons::Cons,
        env::{Env, sym},
        error::{Type, TypeError},
        gc::{Context, Rt, Rto},
        object::{
            Function, Gc, HashTable, IntoObject, LispHashTable, LispString, LispVec, List,
            ListType, NIL, Object, ObjectType, OptionalFlag, Symbol, StringPropertyRun,
            string_property_runs, copy_string_with_properties,
        },
    },
    data::aref,
    library::filevercmp::filevercmp,
    rooted_iter,
};
use anyhow::{Result, anyhow, bail, ensure};
use base64::Engine;
use fallible_iterator::FallibleIterator;
use fallible_streaming_iterator::FallibleStreamingIterator;
use rune_core::macros::{call, list, rebind, root};
use rune_macros::defun;

#[defun]
fn identity(arg: Object) -> Object {
    arg
}

pub(crate) fn slice_into_list<'ob>(
    slice: &[Object<'ob>],
    tail: Option<Object<'ob>>,
    cx: &'ob Context,
) -> Object<'ob> {
    let from_end = slice.iter().rev();
    from_end.fold(tail.into(), |acc, obj| Cons::new(*obj, acc, cx).into())
}

pub(crate) fn build_list<'ob, E>(
    mut iter: impl Iterator<Item = Result<Object<'ob>, E>>,
    cx: &'ob Context,
) -> Result<Object<'ob>, E> {
    let Some(first) = iter.next() else { return Ok(NIL) };
    let head = Cons::new1(first?, cx);
    let mut prev = head;
    for elem in iter {
        let new = Cons::new1(elem?, cx);
        prev.set_cdr(new.into()).unwrap();
        prev = new;
    }
    Ok(head.into())
}

#[defun]
pub(crate) fn eq(obj1: Object, obj2: Object) -> bool {
    obj1.ptr_eq(obj2)
}

#[defun]
pub(crate) fn equal<'ob>(obj1: Object<'ob>, obj2: Object<'ob>) -> bool {
    obj1 == obj2
}

#[defun]
pub(crate) fn eql<'ob>(obj1: Object<'ob>, obj2: Object<'ob>) -> bool {
    match (obj1.untag(), obj2.untag()) {
        (ObjectType::Float(f1), ObjectType::Float(f2)) => f1.to_bits() == f2.to_bits(),
        (ObjectType::BigInt(left), ObjectType::BigInt(right)) => (**left).eq(&**right),
        (ObjectType::Int(integer), ObjectType::BigInt(big))
        | (ObjectType::BigInt(big), ObjectType::Int(integer)) => {
            num_traits::ToPrimitive::to_i64(&**big) == Some(integer)
        }
        _ => obj1.ptr_eq(obj2),
    }
}

#[defun]
fn equal_including_properties<'ob>(o1: Object<'ob>, o2: Object<'ob>) -> bool {
    // TODO: implement text properties
    equal(o1, o2)
}

#[defun]
pub fn plist_get<'ob>(plist: Object<'ob>, prop: Object<'ob>) -> Result<Object<'ob>> {
    let Ok(plist) = Gc::<ListType>::try_from(plist) else { return Ok(NIL) };
    // TODO: this function should never fail. Need to implement safe iterator
    let mut iter = plist.elements();
    while let Some(cur_prop) = iter.next() {
        let Some(value) = iter.next() else { return Ok(NIL) };
        if eq(cur_prop?, prop) {
            return Ok(value?);
        }
    }
    Ok(NIL)
}

/// Mutate the first eq-matching value cell, or append a new property pair.
#[defun]
pub(crate) fn plist_put<'ob>(
    plist: Object<'ob>,
    prop: Object<'ob>,
    value: Object<'ob>,
    predicate: Option<Object>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    ensure!(predicate.is_none(), "plist-put predicate support not implemented");
    let list: List = plist.try_into()?;
    let mut cells = list.conses();
    let mut last_value: Option<&Cons> = None;
    while let Some(key) = cells.next() {
        let key = key?;
        let value_cell = cells.next().ok_or_else(|| anyhow!("Odd length property list"))??;
        if eq(key.car(), prop) {
            value_cell.set_car(value)?;
            return Ok(plist);
        }
        last_value = Some(value_cell);
    }
    let pair = Cons::new(prop, Cons::new(value, NIL, cx), cx);
    if let Some(last_value) = last_value {
        last_value.set_cdr(pair.into())?;
        Ok(plist)
    } else {
        Ok(pair.into())
    }
}

#[defun]
fn plist_member<'ob>(
    plist: Object<'ob>,
    prop: Object<'ob>,
    predicate: Option<Object>,
) -> Result<Object<'ob>> {
    ensure!(predicate.is_none(), "plist-member predicate support not implemented");
    let plist: List = plist.try_into()?;
    for (idx, value) in plist.conses().enumerate() {
        if idx % 2 != 0 {
            continue;
        }
        let value = value?;
        if eq(value.car(), prop) {
            return Ok(value.into());
        }
    }
    Ok(NIL)
}

#[defun]
pub(crate) fn prin1_to_string(object: Object, _noescape: Option<Object>) -> Result<String> {
    // Native rendering is callback-free. Keep mutable global hash insertion
    // excluded from both validation and the following legacy printer walk.
    let _symbols = crate::core::env::INTERNED_SYMBOLS.lock().unwrap();
    crate::property_guard::reject_property_strings(object)?;
    Ok(format!("{object}"))
}

#[defun]
fn string_to_multibyte(string: &LispString) -> &LispString {
    // TODO: Handle the unibyte case
    string
}

#[defun(name = "string-search")]
fn string_search_value(needle: Object, haystack: Object, start_pos: Option<Object>, env: &mut Rt<Env>, cx: &Context) -> Result<Option<usize>> {
    // All three inputs remain Objects until ordered validation is complete.
    // Generated argument conversion must not reject START before either string.
    for value in [needle, haystack] {
        if !matches!(value.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
            env.initialize_condition_metadata(cx);
            let data = list!(sym::STRINGP, value; cx);
            return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into());
        }
    }
    let start_pos: Option<usize> = match start_pos.filter(|value| !value.is_nil()) {
        None => None,
        Some(value) if matches!(value.untag(), ObjectType::Int(_)) => Some(value.try_into()?),
        Some(value) => {
            env.initialize_condition_metadata(cx);
            let predicate = crate::core::env::intern("fixnump", cx);
            let data = list!(predicate, value; cx);
            return Err(crate::eval::EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into());
        }
    };
    let start = start_pos.unwrap_or(0);
    match (needle.untag(), haystack.untag()) {
        (ObjectType::String(needle), ObjectType::String(haystack)) => string_search(needle, haystack, start_pos),
        (ObjectType::ByteString(needle), ObjectType::ByteString(haystack)) => byte_string_search(needle, haystack, start),
        (ObjectType::ByteString(needle), ObjectType::String(haystack)) => {
            ensure!(start <= haystack.chars().count(), "string-search start out of range");
            if !needle.is_ascii() { return Ok(None); }
            string_search(std::str::from_utf8(needle)?, haystack, start_pos)
        }
        (ObjectType::String(needle), ObjectType::ByteString(haystack)) => {
            ensure!(start <= haystack.len(), "string-search start out of range");
            if !needle.is_ascii() { return Ok(None); }
            byte_string_search(needle.as_bytes(), haystack, start)
        }
        (other, _) if !matches!(other, ObjectType::String(_) | ObjectType::ByteString(_)) => Err(TypeError::new(Type::String, other).into()),
        (_, other) => Err(TypeError::new(Type::String, other).into()),
    }
}
fn byte_string_search(needle: &[u8], haystack: &[u8], start: usize) -> Result<Option<usize>> {
    ensure!(start <= haystack.len(), "string-search start out of range");
    if needle.is_empty() { return Ok(Some(start)); }
    if needle.len() > haystack.len() - start { return Ok(None); }
    // Prefix fallback keeps repeated-prefix byte inputs linear without decoding.
    let mut prefix = Vec::new();
    prefix.try_reserve_exact(needle.len())?;
    prefix.resize(needle.len(), 0usize);
    for index in 1..needle.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && needle[index] != needle[matched] { matched = prefix[matched - 1]; }
        if needle[index] == needle[matched] { matched += 1; }
        prefix[index] = matched;
    }
    let mut matched = 0;
    for (offset, byte) in haystack[start..].iter().copied().enumerate() {
        while matched > 0 && byte != needle[matched] { matched = prefix[matched - 1]; }
        if byte == needle[matched] { matched += 1; }
        if matched == needle.len() { return Ok(Some(start + offset + 1 - needle.len())); }
    }
    Ok(None)
}

fn string_search(needle: &str, haystack: &str, start_pos: Option<usize>) -> Result<Option<usize>> {
    let start = start_pos.unwrap_or(0);
    let char_len = haystack.chars().count();
    ensure!(start <= char_len, "string-search start out of range");
    let start_byte =
        haystack.char_indices().nth(start).map_or(haystack.len(), |(offset, _)| offset);
    Ok(haystack[start_byte..]
        .find(needle)
        .map(|offset| start + haystack[start_byte..start_byte + offset].chars().count()))
}

#[defun]
pub(crate) fn mapcar<'ob>(
    function: &Rto<Function>,
    sequence: &Rto<Object>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    let sequence = sequence.bind(cx);
    match sequence.untag() {
        ObjectType::NIL => Ok(NIL),
        ObjectType::Cons(cons) => {
            rooted_iter!(iter, cons, cx);
            root!(outputs, new(Vec), cx);
            while let Some(obj) = iter.next()? {
                let output = call!(function, obj; env, cx)?;
                outputs.push(output);
            }
            // TODO: remove this intermediate vector
            Ok(slice_into_list(Rt::bind_slice(outputs, cx), None, cx))
        }
        ObjectType::Vec(_)
        | ObjectType::String(_)
        | ObjectType::ByteString(_)
        | ObjectType::ByteFn(_) => {
            let len = length(sequence)?;
            root!(array, sequence, cx);
            root!(outputs, new(Vec), cx);
            for i in 0..len {
                let val = aref(array.bind(cx), i, cx)?;
                let output = call!(function, val; env, cx)?;
                outputs.push(output);
            }
            Ok(slice_into_list(Rt::bind_slice(outputs, cx), None, cx))
        }
        _ => Err(TypeError::new(Type::Sequence, sequence).into()),
    }
}

#[defun]
pub(crate) fn mapc<'ob>(
    function: &Rto<Function>,
    sequence: &Rto<Object>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    match sequence.bind(cx).untag() {
        ObjectType::NIL => {}
        ObjectType::Cons(cons) => {
            rooted_iter!(elements, cons, cx);
            while let Some(elem) = elements.next()? {
                call!(function, elem; env, cx)?;
            }
        }
        ObjectType::Vec(_)
        | ObjectType::String(_)
        | ObjectType::ByteString(_)
        | ObjectType::ByteFn(_) => {
            let len = length(sequence.bind(cx))?;
            for i in 0..len {
                let val = aref(sequence.bind(cx), i, cx)?;
                call!(function, val; env, cx)?;
            }
        }
        _ => return Err(TypeError::new(Type::Sequence, sequence.bind(cx)).into()),
    }
    Ok(sequence.bind(cx))
}

#[defun]
pub(crate) fn mapcan<'ob>(
    function: &Rto<Function>,
    sequence: &Rto<Object>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    let mapped = mapcar(function, sequence, env, cx)?;
    let mut lists = Vec::new();
    for list in mapped.as_list()? {
        lists.push(list?.try_into()?);
    }
    nconc(&lists)
}

#[defun]
pub(crate) fn mapconcat(
    function: &Rto<Function>,
    sequence: &Rto<Object>,
    separator: Option<&Rto<Object>>,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<String> {
    let mapped = rebind!(mapcar(function, sequence, env, cx)?);
    let mut string = String::new();
    let mut first = true;
    for element in mapped.as_list()? {
        if !first {
            if let Some(separator) = separator {
                reject_string_properties(separator.bind(cx))?;
                append_character_sequence(&mut string, separator.bind(cx))?;
            }
        }
        first = false;
        let element = element?;
        reject_string_properties(element)?;
        append_character_sequence(&mut string, element)?;
    }
    Ok(string)
}

#[defun]
pub(crate) fn nreverse(seq: Object) -> Result<Object> {
    match seq.untag() {
        ObjectType::NIL => Ok(NIL),
        ObjectType::Cons(cons) => {
            let mut prev = NIL;
            for tail in cons.conses() {
                let tail = tail?;
                tail.set_cdr(prev)?;
                prev = tail.into();
            }
            Ok(prev)
        }
        ObjectType::Vec(vector) => {
            if vector.len() < 2 {
                return Ok(seq);
            }
            let cells = vector.try_mut()?;
            for index in 0..cells.len() / 2 {
                let opposite = cells.len() - index - 1;
                let first = cells[index].get();
                let last = cells[opposite].get();
                cells[index].set(last);
                cells[opposite].set(first);
            }
            Ok(seq)
        }
        _ => bail!(TypeError::new(Type::Sequence, seq)),
    }
}

#[defun]
pub(crate) fn reverse<'ob>(seq: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    reject_string_properties(seq)?;
    match seq.untag() {
        ObjectType::NIL => Ok(NIL),
        ObjectType::Cons(cons) => {
            let mut tail = NIL;
            for elem in cons {
                tail = Cons::new(elem?, tail, cx).into();
            }
            Ok(tail)
        }
        ObjectType::Vec(vector) => {
            let values: Vec<Object> = vector.iter().rev().map(|cell| cell.get()).collect();
            Ok(values.into_obj(cx).into())
        }
        ObjectType::String(string) if string.is_ascii() => {
            Ok(cx.add(string.as_bytes().iter().rev().copied().collect::<Vec<u8>>()))
        }
        ObjectType::String(string) => Ok(cx.add(string.chars().rev().collect::<String>())),
        ObjectType::ByteString(string) => {
            Ok(cx.add(string.iter().rev().copied().collect::<Vec<u8>>()))
        }
        _ => bail!(TypeError::new(Type::Sequence, seq)),
    }
}

#[defun]
pub(crate) fn nconc<'ob>(lists: &[Object<'ob>]) -> Result<Object<'ob>> {
    let mut head = NIL;
    let mut tail: Option<&Cons> = None;
    for (index, &list) in lists.iter().enumerate() {
        if list == NIL {
            continue;
        }
        if head == NIL {
            head = list;
        }
        if let Some(previous) = tail {
            previous.set_cdr(list)?;
        }
        // The final argument is shared and may be any object.
        if index + 1 == lists.len() {
            return Ok(head);
        }
        let cons = list.cons().ok_or_else(|| TypeError::new(Type::List, list))?;
        for next in cons.conses() {
            match next {
                Ok(cons) => tail = Some(cons),
                // GNU nconc replaces the last cdr even in a dotted prefix.
                Err(crate::core::cons::ConsError::NonNilCdr) => break,
                Err(error) => return Err(error.into()),
            }
        }
    }
    // Trailing nil arguments must terminate the concatenated list.
    if let Some(tail) = tail {
        tail.set_cdr(NIL)?;
    }
    Ok(head)
}

fn join<'ob>(list: &mut Vec<Object<'ob>>, seq: List<'ob>) -> Result<()> {
    if let Some(cons) = seq.cons() {
        for elt in cons {
            list.push(elt?);
        }
    }
    Ok(())
}

#[defun]
fn take<'ob>(n: i64, list: List<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    let Ok(n) = usize::try_from(n) else { return Ok(NIL) };
    Ok(build_list(list.elements().take(n), cx)?)
}

#[defun]
pub(crate) fn append<'ob>(sequences: &[Object<'ob>], cx: &'ob Context) -> Result<Object<'ob>> {
    let Some((tail, sequences)) = sequences.split_last() else { return Ok(NIL) };
    let mut elements: Vec<Object<'ob>> = Vec::new();
    for sequence in sequences {
        match sequence.untag() {
            ObjectType::String(string) => {
                elements.extend(string.chars().map(|ch| Object::from(ch as i64)));
            }
            ObjectType::ByteString(string) => {
                elements.extend(string.iter().map(|byte| Object::from(*byte as i64)));
            }
            ObjectType::Vec(vector) => elements.extend(vector.iter().map(|cell| cell.get())),
            ObjectType::NIL | ObjectType::Cons(_) => join(&mut elements, (*sequence).try_into()?)?,
            _ => bail!(TypeError::new(Type::Sequence, *sequence)),
        }
    }
    // The last argument is shared, not copied, and may be any object.
    Ok(slice_into_list(&elements, Some(*tail), cx))
}

#[defun]
pub(crate) fn assq<'ob>(key: Object<'ob>, alist: List<'ob>) -> Result<Object<'ob>> {
    for elem in alist {
        if let Some(cons) = elem?.cons()
            && eq(key, cons.car())
        {
            return Ok(cons.into());
        }
    }
    Ok(NIL)
}

#[defun]
fn rassq<'ob>(key: Object<'ob>, alist: List<'ob>) -> Result<Object<'ob>> {
    for elem in alist {
        if let Some(cons) = elem?.cons()
            && eq(key, cons.cdr())
        {
            return Ok(cons.into());
        }
    }
    Ok(NIL)
}

/// Return the first alist entry whose complete cdr is equal to KEY.
#[defun]
fn rassoc<'ob>(key: Object<'ob>, alist: List<'ob>) -> Result<Object<'ob>> {
    for element in alist {
        if let Some(entry) = element?.cons()
            && (eq(key, entry.cdr()) || equal(key, entry.cdr()))
        {
            return Ok(entry.into());
        }
    }
    Ok(NIL)
}

#[defun]
pub(crate) fn assoc<'ob>(
    key: &Rto<Object<'ob>>,
    alist: &Rto<List<'ob>>,
    testfn: Option<&Rto<Object>>,
    cx: &'ob mut Context,
    env: &mut Rt<Env>,
) -> Result<Object<'ob>> {
    match testfn {
        Some(x) => {
            let func: Function = x.bind(cx).try_into()?;
            root!(func, cx);
            rooted_iter!(iter, alist, cx);
            while let Some(elem) = iter.next()? {
                if let Some(cons) = elem.bind(cx).cons() {
                    let val = cons.car();
                    root!(cons, cx);
                    let result = call!(func, key, val; env, cx)?;
                    if result != NIL {
                        return Ok(cons.bind(cx).into());
                    }
                }
            }
        }
        None => {
            let alist = alist.bind(cx);
            let key = key.bind(cx);
            for elem in alist {
                if let Some(cons) = elem?.cons()
                    && equal(key, cons.car())
                {
                    return Ok(cons.into());
                }
            }
        }
    };
    Ok(NIL)
}

type EqFunc = for<'ob> fn(Object<'ob>, Object<'ob>) -> bool;

#[defun]
fn copy_alist<'ob>(alist: List<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    match alist.untag() {
        ListType::Nil => Ok(NIL),
        ListType::Cons(cons) => {
            let first = copy_alist_elem(cons.car(), cx);
            let head = Cons::new1(first, cx);
            let mut tail = head;

            for elem in cons.cdr().as_list()? {
                let elem = copy_alist_elem(elem?, cx);
                let copy = Cons::new1(elem, cx);
                tail.set_cdr(copy.into()).unwrap();
                tail = copy;
            }
            Ok(head.into())
        }
    }
}

fn copy_alist_elem<'ob>(elem: Object<'ob>, cx: &'ob Context) -> Object<'ob> {
    match elem.untag() {
        ObjectType::Cons(cons) => Cons::new(cons.car(), cons.cdr(), cx).into(),
        _ => elem,
    }
}

fn delete_from_list<'ob>(elt: Object<'ob>, list: List<'ob>, eq_fn: EqFunc) -> Result<Object<'ob>> {
    let mut head = list.into();
    let mut prev: Option<&'ob Cons> = None;
    for tail in list.conses() {
        let tail = tail?;
        if eq_fn(tail.car(), elt) {
            if let Some(prev_tail) = &mut prev {
                prev_tail.set_cdr(tail.cdr())?;
            } else {
                head = tail.cdr();
            }
        } else {
            prev = Some(tail);
        }
    }
    Ok(head)
}

#[defun]
pub(crate) fn delete<'ob>(elt: Object<'ob>, list: List<'ob>) -> Result<Object<'ob>> {
    delete_from_list(elt, list, equal)
}

#[defun]
pub(crate) fn delq<'ob>(elt: Object<'ob>, list: List<'ob>) -> Result<Object<'ob>> {
    delete_from_list(elt, list, eq)
}

fn member_of_list<'ob>(elt: Object<'ob>, list: List<'ob>, eq_fn: EqFunc) -> Result<Object<'ob>> {
    let val = list.conses().fallible().find(|x| Ok(eq_fn(x.car(), elt)))?;
    match val {
        Some(elem) => Ok(elem.into()),
        None => Ok(NIL),
    }
}

#[defun]
pub(crate) fn memq<'ob>(elt: Object<'ob>, list: List<'ob>) -> Result<Object<'ob>> {
    member_of_list(elt, list, eq)
}

#[defun]
pub(crate) fn memql<'ob>(elt: Object<'ob>, list: List<'ob>) -> Result<Object<'ob>> {
    member_of_list(elt, list, eql)
}

#[defun]
pub(crate) fn member<'ob>(elt: Object<'ob>, list: List<'ob>) -> Result<Object<'ob>> {
    member_of_list(elt, list, equal)
}

// TODO: Handle sorting vectors
#[defun]
fn sort<'ob>(
    seq: &Rto<List>,
    predicate: &Rto<Function>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    let vec: Vec<_> = seq.bind(cx).elements().fallible().collect()?;
    if vec.len() <= 1 {
        return Ok(seq.bind(cx).into());
    }
    root!(vec, cx);
    let mut err = None;
    // TODO: Should we specialize some common predicates (<, >, string<, etc)?
    vec.sort_by(|a, b| {
        use std::cmp::Ordering;
        if err.is_some() {
            // We previously hit an error and don't want to call predicate
            // anymore, but still need to wait for sort to finish.
            return Ordering::Equal;
        }
        let result = call!(predicate, a, b; env, cx);
        match result {
            Ok(x) if x == NIL => Ordering::Greater,
            Ok(_) => Ordering::Less,
            Err(e) => {
                err = Some(e.into());
                Ordering::Equal
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(slice_into_list(Rt::bind_slice(vec, cx), None, cx)),
    }
}

defsym!(CYCLIC_VARIABLE_INDIRECTION);
defsym!(VARIABLE_DOCUMENTATION);

#[defun]
pub(crate) fn defvaralias<'ob>(
    new_alias: Symbol,
    base_variable: Symbol<'ob>,
    docstring: Option<Object>,
    env: &mut Rt<Env>,
    cx: &Context,
) -> Result<Symbol<'ob>> {
    let doc = docstring.unwrap_or(NIL);
    if !doc.is_nil() && !matches!(doc.untag(),ObjectType::String(_) | ObjectType::ByteString(_)) {
        return Err(TypeError::new(Type::String,doc).into());
    }
    env.define_variable_alias(new_alias, base_variable, doc, cx)?;
    Ok(base_variable)
}

#[defun]
fn indirect_variable<'ob>(variable: Symbol, env: &Rt<Env>, cx: &'ob Context) -> Result<Symbol<'ob>> {
    env.indirect_variable(variable, cx)
}

/// String-valued documentation properties, including variable alias inheritance.
/// External DOC offsets and command-key substitution require separate support.
#[defun]
fn documentation_property<'ob>(
    symbol: Symbol, property: Symbol, raw: OptionalFlag, env: &Rt<Env>, cx: &'ob Context,
) -> Result<Object<'ob>> {
    let value = env.variable_documentation_property(symbol,property,cx)?;
    if value.is_nil() { return Ok(value); }
    let needs_keys = match value.untag() {
        ObjectType::String(text) => [r"\[",r"\{",r"\<"].iter().any(|marker| text.contains(marker)),
        ObjectType::ByteString(bytes) => bytes.windows(2).any(|pair| pair[0] == b'\\' && matches!(pair[1],b'[' | b'{' | b'<')),
        _ => bail!("Non-string documentation properties are not supported"),
    };
    if raw.is_none() && needs_keys {
        bail!("Documentation command-key substitution is not supported");
    }
    Ok(value)
}

/// Clear only the global special flag; per-load lexical declarations are separate.
#[defun]
fn internal_make_var_non_special(symbol: Symbol) {
    symbol.make_non_special();
}

defsym!(SUBFEATURES);

#[defun]
pub(crate) fn featurep(
    feature: Symbol,
    subfeature: Option<Object>,
    env: &Rt<Env>,
    cx: &Context,
) -> Result<bool> {
    let present = env.has_feature(feature, cx);
    if !present {
        return Ok(false);
    }
    let Some(subfeature) = subfeature.filter(|value| !value.is_nil()) else {
        return Ok(true);
    };
    let subfeatures = crate::data::get(feature, sym::SUBFEATURES, env, cx);
    Ok(!member(subfeature, subfeatures.try_into()?)?.is_nil())
}

#[defun]
pub(crate) fn require<'ob>(
    feature: &Rto<Gc<Symbol>>,
    filename: Option<&Rto<Gc<&LispString>>>,
    noerror: OptionalFlag,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Symbol<'ob>> {
    if env.has_feature(feature.untag(cx), cx) {
        return Ok(feature.untag(cx));
    }
    let file = match filename {
        Some(file) => file.untag(cx),
        None => feature.untag(cx).get().name(),
    };
    let file = file.into_obj(cx);
    root!(file, cx);
    match crate::lread::load(file, noerror, None, cx, env) {
        Ok(_) => Ok(feature.untag(cx)),
        Err(e) => Err(e),
    }
}

// Copy only plist spines: property values retain Lisp eq identity. Shared Context
// access prevents collection/callbacks while these native descriptors are staged.
fn copied_property_runs<'a>(source: Object<'a>, start: usize, end: usize, offset: usize, cx: &'a Context) -> Result<Vec<StringPropertyRun<'a>>> {
    let mut result = Vec::new();
    for run in string_property_runs(source)? {
        let a = run.start.max(start);
        let b = run.end.min(end);
        if a >= b { continue; }
        let mut values = Vec::new();
        for value in run.plist.as_list()? {
            ensure!(values.len() < 8192, "String property plist exceeds copy limit");
            values.push(value?);
        }
        let plist = slice_into_list(&values, None, cx);
        result.push(StringPropertyRun {
            start: offset.checked_add(a - start).ok_or_else(|| anyhow!("String property offset overflow"))?,
            end: offset.checked_add(b - start).ok_or_else(|| anyhow!("String property offset overflow"))?,
            plist,
        });
    }
    Ok(result)
}
fn concat_properties<'a>(output: Object<'a>, sequences: &[Object<'a>], cx: &'a Context) -> Result<Object<'a>> {
    let mut runs = Vec::new();
    let mut offset = 0usize;
    for &sequence in sequences {
        let count = length(sequence)?;
        if matches!(sequence.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
            runs.extend(copied_property_runs(sequence, 0, count, offset, cx)?);
        }
        offset = offset.checked_add(count).ok_or_else(|| anyhow!("String length overflow"))?;
    }
    if runs.is_empty() { return Ok(output); }
    // SAFETY: output and every shallow property value are from this owning
    // Context's live arguments, with no callback or GC during preparation.
    unsafe { copy_string_with_properties(output, &runs, cx) }
}

#[defun(name = "concat")]
pub(crate) fn concat_lisp<'ob>(sequences: &[Object<'ob>], cx: &'ob Context) -> Result<Object<'ob>> {
    let has_bytes = sequences.iter().any(|value| matches!(value.untag(), ObjectType::ByteString(_)));
    if !has_bytes { return concat_properties(cx.add(concat_text(sequences)?), sequences, cx); }
    // An ASCII multibyte accumulator such as mu4e's initial empty string must
    // not reinterpret process bytes as Unicode. Mixed non-ASCII multibyte/raw
    // strings need Emacs's non-scalar character representation, not Latin-1.
    let mut bytes = Vec::new();
    for &sequence in sequences {
        match sequence.untag() {
            ObjectType::ByteString(value) => bytes.extend_from_slice(value),
            ObjectType::String(value) => {
                ensure!(value.is_ascii(), "concat mixing raw bytes and non-ASCII Unicode is not implemented");
                bytes.extend_from_slice(value.as_bytes());
            }
            _ => {
                let mut text = String::new();
                append_character_sequence(&mut text, sequence)?;
                for character in text.chars() {
                    ensure!(character.is_ascii(), "concat mixing raw bytes and non-ASCII character sequences is not implemented");
                    bytes.push(character as u8);
                }
            }
        }
    }
    concat_properties(cx.add(bytes), sequences, cx)
}

pub(crate) fn concat(sequences: &[Object]) -> Result<String> {
    for &sequence in sequences {
        reject_string_properties(sequence)?;
    }
    concat_text(sequences)
}
fn reject_string_properties(value: Object) -> Result<()> {
    let bearing = match value.untag() {
        ObjectType::String(text) => text.has_properties(),
        ObjectType::ByteString(text) => text.has_properties(),
        _ => false,
    };
    ensure!(!bearing, "This string transform does not yet preserve text properties");
    Ok(())
}
fn concat_text(sequences: &[Object]) -> Result<String> {
    let mut result = String::new();
    for &sequence in sequences {
        append_character_sequence(&mut result, sequence)?;
    }
    Ok(result)
}

/// Append characters without interpreting sequence elements as bytes or slicing UTF-8.
/// Rune's current string representation supports Unicode scalar values only.
fn append_character_sequence(output: &mut String, sequence: Object) -> Result<()> {
    let mut append_character = |value: Object| -> Result<()> {
        let ObjectType::Int(value) = value.untag() else {
            bail!("concat sequence element must be a character integer");
        };
        let character = u32::try_from(value)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| anyhow!("concat character {value} is outside Rune's Unicode range"))?;
        output.push(character);
        Ok(())
    };
    match sequence.untag() {
        ObjectType::String(string) => output.push_str(string),
        ObjectType::NIL => {}
        ObjectType::Cons(cons) => {
            for element in cons {
                append_character(element?)?;
            }
        }
        ObjectType::Vec(vector) => {
            for element in vector.iter() {
                append_character(element.get())?;
            }
        }
        // Raw-byte multibyteness needs the string-representation rewrite.
        ObjectType::ByteString(_) => bail!("concat unibyte strings are not supported yet"),
        _ => bail!(TypeError::new(Type::Sequence, sequence)),
    }
    Ok(())
}

#[defun]
pub(crate) fn vconcat<'ob>(sequences: &[Object], cx: &'ob Context) -> Result<Gc<&'ob LispVec>> {
    let mut concated: Vec<Object> = Vec::new();
    for elt in sequences {
        match elt.untag() {
            // TODO: need to correctly handle unibyte strings (no unicode codepoints)
            ObjectType::String(string) => {
                for chr in string.chars() {
                    concated.push((chr as i64).into());
                }
            }
            ObjectType::Cons(cons) => {
                for x in cons {
                    concated.push(x?);
                }
            }
            ObjectType::Vec(vec) => {
                for x in vec.iter() {
                    concated.push(x.get());
                }
            }
            ObjectType::NIL => {}
            obj => bail!(TypeError::new(Type::Sequence, obj)),
        }
    }
    Ok(concated.into_obj(cx))
}

#[defun]
pub(crate) fn length(sequence: Object) -> Result<usize> {
    let size = match sequence.untag() {
        ObjectType::Cons(x) => x.elements().len()?,
        ObjectType::Vec(x) => x.len(),
        ObjectType::String(x) => x.len(),
        ObjectType::ByteString(x) => x.len(),
        ObjectType::ByteFn(x) => x.len(),
        ObjectType::NIL => 0,
        obj => bail!(TypeError::new(Type::Sequence, obj)),
    };
    Ok(size)
}

/// GNU length_internal counts cons cells only, stopping once LIMIT is reached.
/// A negative result means the list has at least LIMIT cells.
fn list_length_remainder(cons: &Cons, mut limit: i128) -> Result<i128> {
    if limit < 0xffff {
        // Small bounds deliberately terminate even on circular lists.
        let mut tail: Object = cons.into();
        while let Some(cons) = tail.cons() {
            limit -= 1;
            if limit <= 0 {
                return Ok(-1);
            }
            tail = cons.cdr();
        }
    } else {
        for cell in cons.conses() {
            match cell {
                Ok(_) => {
                    limit -= 1;
                    if limit <= 0 {
                        return Ok(-1);
                    }
                }
                Err(crate::core::cons::ConsError::NonNilCdr) => break,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(limit)
}

#[defun(name = "length=")]
fn length_equal(sequence: Object, target: i64) -> Result<bool> {
    // GNU returns nil before examining SEQUENCE for a negative target.
    if target < 0 {
        return Ok(false);
    }
    if let ObjectType::Cons(cons) = sequence.untag() {
        return Ok(list_length_remainder(cons, target as i128 + 1)? == 1);
    }
    Ok(length(sequence)? as i128 == target as i128)
}

#[defun(name = "length<")]
fn length_less(sequence: Object, target: i64) -> Result<bool> {
    if let ObjectType::Cons(cons) = sequence.untag() {
        return Ok(list_length_remainder(cons, target as i128)? != -1);
    }
    Ok((length(sequence)? as i128) < target as i128)
}

#[defun(name = "length>")]
fn length_greater(sequence: Object, target: i64) -> Result<bool> {
    if let ObjectType::Cons(cons) = sequence.untag() {
        return Ok(list_length_remainder(cons, target as i128 + 1)? == -1);
    }
    Ok((length(sequence)? as i128) > target as i128)
}

#[defun]
pub(crate) fn safe_length(sequence: Object) -> usize {
    match sequence.untag() {
        ObjectType::Cons(cons) => {
            let mut count = 0_usize;
            for element in cons.elements() {
                match element {
                    Ok(_) => count += 1,
                    // Floyd detection can meet before visiting every distinct
                    // cell in a prefixed cycle. Twice the visited count is an
                    // upper bound, satisfying GNU safe-length's guarantee.
                    Err(crate::core::cons::ConsError::CircularList) => {
                        return count.saturating_mul(2);
                    }
                    Err(crate::core::cons::ConsError::NonNilCdr) => return count,
                }
            }
            count
        }
        _ => 0,
    }
}

#[defun]
pub(crate) fn proper_list_p(object: Object) -> Option<usize> {
    match object.untag() {
        ObjectType::Cons(x) => x.elements().len().ok(),
        ObjectType::NIL => Some(0),
        _ => None,
    }
}

#[defun]
pub(crate) fn nth(n: i64, list: List) -> Result<Object> {
    Ok(crate::data::car(nthcdr(n, list)?.try_into()?))
}

#[defun]
pub(crate) fn nthcdr(n: i64, list: List) -> Result<Object> {
    let mut tail = list.as_obj_copy();
    for _ in 0..n {
        match tail.untag() {
            ObjectType::Cons(cons) => tail = cons.cdr(),
            ObjectType::NIL => return Ok(NIL),
            _ => bail!(TypeError::new(Type::List, tail)),
        }
    }
    Ok(tail)
}

#[defun]
pub(crate) fn elt<'ob>(sequence: Object<'ob>, n: usize, cx: &'ob Context) -> Result<Object<'ob>> {
    match sequence.untag() {
        ObjectType::Cons(x) => nth(n.try_into()?, x.into()),
        ObjectType::NIL => Ok(NIL),
        ObjectType::Vec(x) => aref(x.into(), n, cx),
        ObjectType::Record(x) => aref(x.into(), n, cx),
        ObjectType::String(x) => aref(x.into(), n, cx),
        ObjectType::ByteFn(x) => aref(x.into(), n, cx),
        other => Err(TypeError::new(Type::Sequence, other).into()),
    }
}

#[defun]
pub(crate) fn string_equal<'ob>(s1: Object<'ob>, s2: Object<'ob>) -> Result<bool> {
    let s1 = match s1.untag() {
        ObjectType::String(x) => x.as_bytes(),
        ObjectType::ByteString(x) => x.inner(),
        ObjectType::Symbol(x) => x.get().as_bytes(),
        _ => bail!(TypeError::new(Type::String, s1)),
    };
    let s2 = match s2.untag() {
        ObjectType::String(x) => x.as_bytes(),
        ObjectType::ByteString(x) => x.inner(),
        ObjectType::Symbol(x) => (x.get()).as_bytes(),
        _ => bail!(TypeError::new(Type::String, s2)),
    };

    Ok(s1 == s2)
}

#[defun]
pub(crate) fn compare_strings<'ob>(
    string1: &str,
    start1: Object<'ob>,
    end1: Object<'ob>,
    string2: &str,
    start2: Object<'ob>,
    end2: Object<'ob>,
    ignore_case: OptionalFlag,
) -> Result<Object<'ob>> {
    let bounds = |string: &str, start: Object, end: Object| -> Result<(usize, usize)> {
        let len = string.chars().count() as i128;
        let position = |value: Object, default: i128, clamp_end: bool| -> Result<usize> {
            let value = match value.untag() {
                ObjectType::Int(value) => value as i128,
                ObjectType::NIL => default,
                _ => bail!(TypeError::new(Type::Int, value)),
            };
            let value = if value < 0 { len + value } else { value };
            let value = if clamp_end { value.min(len) } else { value };
            ensure!(value >= 0 && value <= len, "compare-strings args out of range");
            Ok(value as usize)
        };
        let start = position(start, 0, false)?;
        // GNU preserves historical clipping of positive end positions above length.
        let end = position(end, len, true)?;
        ensure!(start <= end, "compare-strings args out of range");
        Ok((start, end))
    };
    let (start1, end1) = bounds(string1, start1, end1)?;
    let (start2, end2) = bounds(string2, start2, end2)?;
    let s1 = string1.chars().skip(start1).take(end1 - start1);
    let s2 = string2.chars().skip(start2).take(end2 - start2);

    let mut s1 = s1;
    let mut s2 = s2;
    let mut leading: i64 = 1;
    loop {
        let (c1, c2) = match (s1.next(), s2.next()) {
            (None, None) => return Ok(true.into()),
            (None, Some(_)) => return Ok((-leading).into()),
            (Some(_), None) => return Ok(leading.into()),
            (Some(c1), Some(c2)) => (c1, c2),
        };
        let (c1, c2) = if ignore_case.is_some() {
            //TODO: use case-table to determine the uppercase of a character
            (c1.to_uppercase().next().unwrap_or(c1), c2.to_uppercase().next().unwrap_or(c2))
        } else {
            (c1, c2)
        };

        match c1.cmp(&c2) {
            std::cmp::Ordering::Less => return Ok((-leading).into()),
            std::cmp::Ordering::Greater => return Ok(leading.into()),
            std::cmp::Ordering::Equal => {}
        }
        leading += 1;
    }
}

#[defun]
pub(crate) fn string_distance(string1: &str, string2: &str, bytecompare: OptionalFlag) -> i64 {
    if bytecompare.is_none() {
        levenshtein_distance(string1.chars(), string2.chars())
    } else {
        levenshtein_distance(string1.as_bytes().iter(), string2.as_bytes().iter())
    }
}

#[inline]
pub(crate) fn levenshtein_distance<T: PartialEq, I: Iterator<Item = T>>(s1: I, s2: I) -> i64 {
    use std::cmp::min;
    // Initialize work vectors
    let s = s1.collect::<Vec<_>>();
    let t = s2.collect::<Vec<_>>();
    let mut v0 = vec![0; t.len() + 1];
    let mut v1 = vec![0; t.len() + 1];

    // Initialize v0
    for (i, v0i) in v0.iter_mut().enumerate() {
        *v0i = i as i64;
    }

    // Calculate v1 from previous row v0
    for (i, si) in s.iter().enumerate() {
        // First element of v1 is A[i+1][0]
        // Edit distance is delete (i+1) chars from s to match empty t
        v1[0] = i as i64 + 1;

        // fills in the rest of the row
        for (j, tj) in t.iter().enumerate() {
            let deletion_cost = v0[j + 1] + 1;
            let insertion_cost = v1[j] + 1;
            let substitution_cost = v0[j] + if si == tj { 0 } else { 1 };
            v1[j + 1] = min(deletion_cost, min(insertion_cost, substitution_cost));
        }

        // Swap v1 and v0
        std::mem::swap(&mut v0, &mut v1);
    }
    // Return the final result
    v0[t.len()]
}

#[defun]
pub(crate) fn string_bytes(string: &str) -> usize {
    string.len()
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum StringOrChar<'ob> {
    String(&'ob str),
    Char(u64),
}

impl<'ob> TryFrom<Object<'ob>> for StringOrChar<'ob> {
    type Error = TypeError;

    fn try_from(obj: Object<'ob>) -> Result<Self, Self::Error> {
        match obj.untag() {
            ObjectType::String(s) => Ok(Self::String(s)),
            ObjectType::Int(c) if c >= 0 => Ok(Self::Char(c as u64)),
            _ => Err(TypeError::new(Type::StringOrChar, obj)),
        }
    }
}

impl<'ob> From<&'ob str> for StringOrChar<'ob> {
    fn from(s: &'ob str) -> Self {
        Self::String(s)
    }
}

impl From<char> for StringOrChar<'_> {
    fn from(s: char) -> Self {
        Self::Char(s as u64)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StringOrSymbol<'ob>(&'ob str);

impl<'ob> TryFrom<Object<'ob>> for StringOrSymbol<'ob> {
    type Error = TypeError;

    fn try_from(obj: Object<'ob>) -> Result<Self, Self::Error> {
        match obj.untag() {
            ObjectType::String(x) => Ok(Self(x)),
            ObjectType::Symbol(x) => Ok(Self(x.get().name())),
            _ => Err(TypeError::new(Type::String, obj)),
        }
    }
}

#[defun]
pub(crate) fn string_lessp<'ob>(
    string1: StringOrSymbol<'ob>,
    string2: StringOrSymbol<'ob>,
) -> Result<bool> {
    for (c1, c2) in string1.0.chars().zip(string2.0.chars()) {
        if c1 != c2 {
            return Ok(c1 < c2);
        }
    }
    Ok(string1.0.len() < string2.0.len())
}

#[defun]
pub(crate) fn string_version_lessp<'ob>(
    string1: StringOrSymbol<'ob>,
    string2: StringOrSymbol<'ob>,
) -> Result<bool> {
    Ok(filevercmp(string1.0.as_bytes(), string2.0.as_bytes()) == std::cmp::Ordering::Less)
}

#[defun]
pub(crate) fn clear_string<'ob>(
    string: &Rto<Gc<&LispString>>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    LispString::clear_rooted(string, cx)?;
    Ok(NIL)
}

///////////////
// HashTable //
///////////////

defsym!(KW_TEST);
defsym!(KW_DOCUMENTATION);

#[defun]
pub(crate) fn make_hash_table<'ob>(
    keyword_args: &[Object<'ob>],
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let kw_test_pos = keyword_args.iter().step_by(2).position(|&x| x == sym::KW_TEST);
    if let Some(i) = kw_test_pos {
        let Some(val) = keyword_args.get((i * 2) + 1) else {
            bail!("Missing keyword value for :test")
        };
        if *val != sym::EQ && *val != sym::EQUAL && *val != sym::EQL {
            // TODO: we are currently only using `equal', but eq should be okay
            bail!("only `eq' and `equal' keywords support for make-hash-table :test. Found {val}");
        }
    }
    // TODO, the rest of the keywords need to be supported here
    let map = HashTable::with_hasher(std::hash::BuildHasherDefault::default());
    Ok(cx.add(map))
}

#[defun]
pub(crate) fn hash_table_p(obj: Object) -> bool {
    matches!(obj.untag(), ObjectType::HashTable(_))
}

#[defun]
pub(crate) fn gethash<'ob>(
    key: Object<'ob>,
    table: &'ob LispHashTable,
    dflt: Option<Object<'ob>>,
) -> Option<Object<'ob>> {
    match table.get(key) {
        Some(x) => Some(x),
        None => dflt,
    }
}

#[defun]
pub(crate) fn puthash<'ob>(
    key: Object<'ob>,
    value: Object<'ob>,
    table: &'ob LispHashTable,
) -> Result<Object<'ob>> {
    table.insert(key, value)?;
    Ok(value)
}

#[defun]
fn remhash(key: Object, table: &LispHashTable) -> Result<()> {
    let Some(idx) = table.get_index_of(key) else { return Ok(()) };
    // If the removed element is before our iterator, then we need to shift the
    // iterator back one because the whole map get's shifted when something is
    // removed.
    let iter_idx = table.get_iter_index();
    if idx < iter_idx {
        table.set_iter_index(iter_idx - 1);
    }
    // TODO: can we use swap_remove?
    table.shift_remove(key);
    Ok(())
}

#[defun]
fn maphash(
    function: &Rto<Function>,
    table: &Rto<Gc<&LispHashTable>>,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<bool> {
    let loop_idx = |table: &LispHashTable| {
        let end = table.len();
        let idx = table.get_iter_index();
        table.set_iter_index(idx + 1);
        if idx >= end { None } else { Some(idx) }
    };

    loop {
        let (key, val) = {
            let table = table.untag(cx);
            let Some(idx) = loop_idx(table) else { break };
            table.get_index(idx).unwrap()
        };
        let result = call!(function, key, val; env, cx);
        if let Err(e) = result {
            table.untag(cx).set_iter_index(0);
            return Err(e.into());
        }
    }
    table.untag(cx).set_iter_index(0);
    Ok(false)
}

#[defun]
fn copy_sequence<'ob>(arg: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    match arg.untag() {
        ObjectType::Vec(x) => Ok(cx.add(x.to_vec())),
        ObjectType::Record(record) => {
            let mut slots = cx.vec_with_capacity(record.len());
            slots.extend(record.iter().map(|cell| cell.get()));
            Ok(crate::core::object::RecordBuilder(slots).into_obj(cx).into())
        }
        ObjectType::ByteString(string) => {
            let runs = copied_property_runs(arg, 0, string.len(), 0, cx)?;
            unsafe { copy_string_with_properties(arg, &runs, cx) }
        },
        ObjectType::Cons(x) => {
            // TODO: remove this temp vector
            let mut elements = Vec::new();
            let mut tail = None;
            for cons in x.conses() {
                let cons = cons?;
                elements.push(cons.car());
                if !matches!(cons.cdr().untag(), ObjectType::Cons(_)) {
                    tail = Some(cons.cdr());
                }
            }
            Ok(slice_into_list(&elements, tail, cx))
        }
        // Calling to_owned on &LispString clones the reference, not its text.
        ObjectType::String(x) => {
            let runs = copied_property_runs(arg, 0, x.chars().count(), 0, cx)?;
            unsafe { copy_string_with_properties(arg, &runs, cx) }
        },
        ObjectType::NIL => Ok(NIL),
        _ => Err(TypeError::new(Type::Sequence, arg).into()),
    }
}

fn substring_object<'a>(string: Object<'a>, from: Option<i64>, to: Option<i64>, preserve: bool, cx: &'a Context) -> Result<Object<'a>> {
    let count = match string.untag() {
        ObjectType::String(text) => text.chars().count(),
        ObjectType::ByteString(bytes) => bytes.len(),
        _ => return Err(TypeError::new(Type::String, string).into()),
    };
    let normalize = |position: i64| -> Result<usize> {
        let value = if position < 0 { count as i128 + i128::from(position) } else { i128::from(position) };
        ensure!(value >= 0 && value <= count as i128, "substring args out of range");
        Ok(value as usize)
    };
    let start = normalize(from.unwrap_or(0))?;
    let end = to.map(normalize).transpose()?.unwrap_or(count);
    ensure!(start <= end, "substring args out of range");
    let output = match string.untag() {
        ObjectType::String(text) => cx.add(text.chars().skip(start).take(end-start).collect::<String>()),
        ObjectType::ByteString(bytes) => cx.add(bytes[start..end].to_vec()),
        _ => unreachable!(),
    };
    if !preserve { return Ok(output); }
    let runs = copied_property_runs(string, start, end, 0, cx)?;
    // SAFETY: new text and copied property spines/values belong to this Context;
    // no collection or callback can intervene through a shared Context borrow.
    unsafe { copy_string_with_properties(output, &runs, cx) }
}
#[defun(name = "substring")]
fn substring_lisp<'a>(string: Object<'a>, from: Option<i64>, to: Option<i64>, cx: &'a Context) -> Result<Object<'a>> {
    substring_object(string, from, to, true, cx)
}
#[defun]
fn substring_no_properties<'a>(string: Object<'a>, from: Option<i64>, to: Option<i64>, cx: &'a Context) -> Result<Object<'a>> {
    substring_object(string, from, to, false, cx)
}

fn substring(string: &str, from: Option<i64>, to: Option<i64>) -> Result<String> {
    let char_len = string.chars().count();
    let normalize = |position: i64| -> Result<usize> {
        let position = if position < 0 {
            char_len as i128 + position as i128
        } else {
            position as i128
        };
        ensure!(position >= 0 && position <= char_len as i128, "substring args out of range");
        Ok(position as usize)
    };
    let from = normalize(from.unwrap_or(0))?;
    let to = match to {
        Some(to) => normalize(to)?,
        None => char_len,
    };
    ensure!(from <= to, "substring args out of range");
    // Lisp indices count characters; only slice at offsets produced by char_indices.
    let byte_offset = |position: usize| {
        string.char_indices().nth(position).map_or(string.len(), |(offset, _)| offset)
    };
    Ok(string[byte_offset(from)..byte_offset(to)].to_owned())
}

defsym!(MD5);
defsym!(SHA1);
defsym!(SHA224);
defsym!(SHA256);
defsym!(SHA384);
defsym!(SHA512);

#[defun]
fn secure_hash_algorithms<'ob>(cx: &'ob Context) -> Object<'ob> {
    // https://crates.io/crates/md-5
    // https://crates.io/crates/sha1
    // https://crates.io/crates/sha2
    // https://crates.io/crates/digest ?
    list![sym::MD5, sym::SHA1, sym::SHA224, sym::SHA256, sym::SHA384, sym::SHA512; cx]
}

#[defun]
fn enable_debug() -> bool {
    crate::debug::enable_debug();
    true
}

#[defun]
fn debug_enabled() -> bool {
    crate::debug::debug_enabled()
}

#[defun]
fn disable_debug() -> bool {
    crate::debug::disable_debug();
    false
}

/// Base64-encode STRING and return the result.
///
/// Optional second argument NO-LINE-BREAK means do not break long lines
/// into shorter lines.
#[defun]
fn base64_encode_string(string: &str, line_break: OptionalFlag) -> Result<String> {
    if string.is_ascii() {
        Ok(base64_encode(string, line_break.is_some(), true, false))
    } else {
        Err(anyhow!("Multibyte character in data for base64 encoding"))
    }
}

/// Base64URL-encode STRING and return the result.
///
/// Optional second argument NO-PAD means do not add padding char =.
///
/// This produces the URL variant of base 64 encoding defined in RFC 4648.
#[defun]
fn base64url_encode_string(string: &str, no_pad: OptionalFlag) -> Result<String> {
    if string.is_ascii() {
        Ok(base64_encode(string, false, no_pad.is_none(), true))
    } else {
        Err(anyhow!("Multibyte character in data for base64 encoding"))
    }
}

fn base64_encode(string: &str, _line_break: bool, pad: bool, base64url: bool) -> String {
    let config = base64::engine::GeneralPurposeConfig::new().with_encode_padding(pad);
    let alphabets = if base64url { base64::alphabet::URL_SAFE } else { base64::alphabet::STANDARD };
    let engine = base64::engine::GeneralPurpose::new(&alphabets, config);
    engine.encode(string)
}

#[cfg(test)]
mod test {
    #[test]
    fn variable_alias_unibyte_documentation_stays_unibyte() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvaralias 'rune-va-byte-doc 'rune-va-byte-doc-base "\377") (let ((doc (documentation-property 'rune-va-byte-doc 'variable-documentation t))) (list (multibyte-string-p doc) (aref doc 0))))"#,
            "(nil 255)",
        );
    }

    #[test]
    fn variable_alias_rejections_use_structured_message_lists_without_mutation() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-va-bound-source 1) (list (condition-case e (defvaralias t 'rune-va-unused) (error e)) (let ((rune-va-bound-source 9)) (condition-case e (defvaralias 'rune-va-bound-source 'rune-va-unused) (error e))) rune-va-bound-source (boundp 'rune-va-unused)))"#,
            r#"((error "Cannot make a constant an alias: t") (error "Don’t know how to make a let-bound variable an alias: rune-va-bound-source") 1 nil)"#,
        );
    }

    #[test]
    fn variable_alias_initialized_defvar_uses_canonical_default() {
        for (source, expected) in [
            (r#"(progn (defvaralias 'rune-va-flags-a 'rune-va-flags-b) (internal-make-var-non-special 'rune-va-flags-b) (list (defvar rune-va-flags-a 7) (special-variable-p 'rune-va-flags-a) (special-variable-p 'rune-va-flags-b) rune-va-flags-a rune-va-flags-b))"#, "(rune-va-flags-a t nil 7 7)"),
            (r#"(progn (defvaralias 'rune-va-dv 'rune-va-dv-base) (list (defvar rune-va-dv 7) rune-va-dv-base (defvar rune-va-dv (error "must skip"))))"#, "(rune-va-dv 7 rune-va-dv)"),
            (r#"(progn (condition-case nil (defvar rune-va-dv-void (error "x")) (error nil)) (list (let ((rune-va-dv-void 9)) (defvaralias 'rune-va-dv-temp 'rune-va-dv-void) rune-va-dv-temp) (boundp 'rune-va-dv-temp)))"#, "(9 nil)"),
            (r#"(progn (condition-case nil (defvar rune-va-dv-saved (error "x")) (error nil)) (list (let ((rune-va-dv-saved 1)) (defvar rune-va-dv-saved 2) rune-va-dv-saved) rune-va-dv-saved))"#, "(1 2)"),
        ] { crate::interpreter::assert_lisp(source,expected); }
    }

    #[test]
    fn variable_alias_uninterned_edges_survive_moving_collection() {
        crate::interpreter::assert_lisp(
            r#"(let ((a (make-symbol "alias")) (b (make-symbol "base"))) (set b 7) (defvaralias a b) (garbage-collect) (list (eq (indirect-variable a) b) (symbol-value a)))"#,
            "(t 7)",
        );
    }
    #[test]
    fn variable_alias_documentation_inherits_only_when_alias_doc_is_nil() {
        crate::interpreter::assert_lisp(
            r#"(progn (defvar rune-va-doc-base 7) (put 'rune-va-doc-base 'variable-documentation "base doc") (defvaralias 'rune-va-doc 'rune-va-doc-base "alias doc") (list (documentation-property 'rune-va-doc 'variable-documentation t) (progn (defvaralias 'rune-va-doc 'rune-va-doc-base nil) (documentation-property 'rune-va-doc 'variable-documentation t))))"#,
            r#"("alias doc" "base doc")"#,
        );
    }
    #[test]
    fn variable_alias_void_and_default_reads_keep_original_name() {
        crate::interpreter::assert_lisp(
            "(progn (defvaralias 'rune-va-void 'rune-va-void-base) (list (condition-case e (symbol-value 'rune-va-void) (void-variable e)) (condition-case e (default-toplevel-value 'rune-va-void) (void-variable e))))",
            "((void-variable rune-va-void) (void-variable rune-va-void))",
        );
    }

    #[test]
    fn variable_alias_value_and_captured_dynamic_base() {
        crate::interpreter::assert_lisp(
            "(progn (setq rune-va-base 7 rune-va-next 11) (defvaralias 'rune-va-alias 'rune-va-base) (list (let ((rune-va-alias 9)) (defvaralias 'rune-va-alias 'rune-va-next) (list rune-va-alias rune-va-base rune-va-next)) rune-va-alias rune-va-base rune-va-next))",
            "((11 9 11) 11 7 11)",
        );
    }
    #[test]
    fn variable_alias_constants_and_cycle_rejection() {
        crate::interpreter::assert_lisp(
            "(progn (defvaralias 'rune-va-constant t) (list rune-va-constant (condition-case e (set 'rune-va-constant 3) (setting-constant e)) (condition-case nil (defvaralias 'rune-va-cycle 'rune-va-cycle) (cyclic-variable-indirection 'caught))))",
            "(t (setting-constant rune-va-constant) caught)",
        );
    }

    #[test]
    fn clearing_global_special_flag_does_not_change_existing_value() {
        crate::interpreter::assert_lisp(
            "(progn (defvar rune-alias-clear-probe 7) (list (special-variable-p 'rune-alias-clear-probe) (internal-make-var-non-special 'rune-alias-clear-probe) (special-variable-p 'rune-alias-clear-probe) (let ((rune-alias-clear-probe 9)) (list rune-alias-clear-probe (symbol-value 'rune-alias-clear-probe)))))",
            "(t nil nil (9 7))",
        );
    }

    #[test]
    fn eql_compares_integer_values_without_erasing_float_identity_bits() {
        use crate::core::gc::{Context, RootSet};
        use crate::interpreter::assert_lisp;
        use num_bigint::BigInt;
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for value in
            [0_i64, 1, -1, crate::core::object::MIN_FIXNUM, crate::core::object::MAX_FIXNUM]
        {
            let boxed = cx.add(BigInt::from(value));
            let immediate = cx.add(value);
            assert!(super::eql(boxed, immediate));
            assert!(super::eql(immediate, boxed));
        }
        drop(cx);
        for (source, expected) in [
            ("(eql (+ 18446744073709551616 1) (+ 18446744073709551616 1))", "t"),
            ("(eql (+ 18446744073709551616 1) (+ 18446744073709551616 2))", "nil"),
            ("(eql 1 1.0)", "nil"),
            ("(eql 0.0 -0.0)", "nil"),
            ("(eql 0.0e+NaN 0.0e+NaN)", "t"),
            ("(eql (list 1) (list 1))", "nil"),
            (
                "(memql (+ 18446744073709551616 1) (list (+ 18446744073709551616 1)))",
                "(18446744073709551617)",
            ),
        ] {
            assert_lisp(source, expected);
        }
    }

    use super::{compare_strings, string_search, substring};
    use crate::core::object::NIL;
    use crate::{
        assert_elprop, fns::levenshtein_distance, interpreter::assert_lisp,
        library::elprop::arb_custom_string,
    };
    use proptest::prelude::*;

    #[test]
    fn compare_strings_prefix_and_character_bounds() {
        assert_lisp("(compare-strings \"aé\" nil nil \"aéx\" nil nil)", "-3");
        assert_lisp("(compare-strings \"aéx\" nil nil \"aé\" nil nil)", "3");
        assert_lisp("(compare-strings \"aé🦀z\" -3 -1 \"é🦀\" nil nil)", "t");
        assert_lisp("(compare-strings \"é\" nil 100 \"é\" nil nil)", "t");
        assert_lisp("(compare-strings \"\" nil nil \"x\" nil nil)", "-1");
    }

    #[test]
    fn compare_strings_rejects_invalid_bounds() {
        for (start, end) in [(-5i64, 4i64), (5, 5), (3, 2), (0, -5), (i64::MIN, i64::MAX)] {
            assert!(
                compare_strings("aé🦀z", start.into(), end.into(), "", NIL, NIL, None).is_err()
            );
            assert!(
                compare_strings("", NIL, NIL, "aé🦀z", start.into(), end.into(), None).is_err()
            );
        }
    }

    #[test]
    fn string_search_uses_character_positions() {
        assert_eq!(string_search("🦀", "aé🦀z", None).unwrap(), Some(2));
        assert_eq!(string_search("z", "aé🦀z", Some(2)).unwrap(), Some(3));
        assert_eq!(string_search("é", "aé🦀z", Some(2)).unwrap(), None);
        assert_eq!(string_search("", "aé🦀z", Some(4)).unwrap(), Some(4));
        assert_eq!(string_search("z", "aé🦀z", Some(4)).unwrap(), None);
        assert_eq!(string_search("", "", None).unwrap(), Some(0));
        assert_eq!(string_search("x", "", None).unwrap(), None);
    }

    #[test]
    fn byte_string_search_matches_reference_without_decoding() {
        let mut state = 0x51a7_u64;
        for case in 0..10_000usize {
            let mut next = || { state = state.wrapping_mul(6364136223846793005).wrapping_add(1); (state >> 32) as u8 };
            let haystack: Vec<u8> = (0..case % 65).map(|_| next() % 4).collect();
            let needle: Vec<u8> = (0..case % 17).map(|_| next() % 4).collect();
            let start = case % (haystack.len() + 1);
            let expected = if needle.is_empty() { Some(start) } else {
                haystack[start..].windows(needle.len()).position(|part| part == needle).map(|offset| start + offset)
            };
            assert_eq!(super::byte_string_search(&needle, &haystack, start).unwrap(), expected);
        }
        let haystack = vec![b'a'; 1024 * 1024];
        let mut needle = vec![b'a'; 4096]; needle.push(b'b');
        assert_eq!(super::byte_string_search(&needle, &haystack, 0).unwrap(), None);
    }

    #[test]
    fn string_search_validates_strings_before_start_like_gnu() {
        for source in [
            r#"(condition-case e (string-search 17 18 'bad) (error e))"#,
            r#"(condition-case e (string-search "a" 18 'bad) (error e))"#,
            r#"(condition-case e (string-search "a" "a" 'bad) (error e))"#,
            r#"(condition-case e (string-search nil 18 'bad) (error e))"#,
        ] { crate::assert_elprop!("{}", source); }
    }

    #[test]
    fn string_search_storage_boundaries_match_gnu() {
        for needle in [r#""""#, r#""a""#, r#""\012""#, r#""\377""#, r#""é""#, r#""\303\251""#] {
            for haystack in [r#""""#, r#""a\nb""#, r#""λa🙂""#, r#""\377a\376""#, r#""é""#, r#""\303\251""#] {
                for start in [0, 1, 2, 5] {
                    crate::assert_elprop!("{}", format!("(condition-case e (string-search {needle} {haystack} {start}) (error 'caught))"));
                }
            }
        }
        crate::interpreter::assert_lisp(r#"(let ((nl (substring "\377\012" 1))) (list (multibyte-string-p nl) (string-search nl "λ\n🙂") (string-search "a" "\377a\376") (string-search "\377" "\377a\376")))"#, "(nil 1 1 0)");
    }

    #[test]
    fn string_search_rejects_invalid_start() {
        assert!(string_search("", "é", Some(2)).is_err());
        assert!(string_search("", "é", Some(usize::MAX)).is_err());
        assert!(string_search("", "", Some(1)).is_err());
    }

    #[test]
    fn substring_uses_character_positions() {
        assert_eq!(substring("aé🦀z", Some(1), Some(3)).unwrap(), "é🦀");
        assert_eq!(substring("aé🦀z", Some(-3), Some(-1)).unwrap(), "é🦀");
        assert_eq!(substring("aé🦀z", None, Some(2)).unwrap(), "aé");
        assert_eq!(substring("aé🦀z", Some(2), None).unwrap(), "🦀z");
        assert_eq!(substring("aé🦀z", None, None).unwrap(), "aé🦀z");
        assert_eq!(substring("aé🦀z", Some(4), None).unwrap(), "");
        assert_eq!(substring("", None, None).unwrap(), "");
    }

    #[test]
    fn substring_rejects_invalid_bounds() {
        for (from, to) in [
            (Some(2), Some(1)),
            (Some(-1), Some(-2)),
            (Some(-5), None),
            (None, Some(5)),
            (Some(i64::MIN), None),
            (None, Some(i64::MAX)),
        ] {
            assert!(substring("aé🦀z", from, to).is_err());
        }
    }

    #[test]
    fn test_base64_encode_string() {
        assert_lisp("(base64-encode-string \"hello\")", "\"aGVsbG8=\"");
        assert_lisp(
            "(base64-encode-string \"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum\")",
            "\"TG9yZW0gaXBzdW0gZG9sb3Igc2l0IGFtZXQsIGNvbnNlY3RldHVyIGFkaXBpc2NpbmcgZWxpdCwgc2VkIGRvIGVpdXNtb2QgdGVtcG9yIGluY2lkaWR1bnQgdXQgbGFib3JlIGV0IGRvbG9yZSBtYWduYSBhbGlxdWEuIFV0IGVuaW0gYWQgbWluaW0gdmVuaWFtLCBxdWlzIG5vc3RydWQgZXhlcmNpdGF0aW9uIHVsbGFtY28gbGFib3JpcyBuaXNpIHV0IGFsaXF1aXAgZXggZWEgY29tbW9kbyBjb25zZXF1YXQuIER1aXMgYXV0ZSBpcnVyZSBkb2xvciBpbiByZXByZWhlbmRlcml0IGluIHZvbHVwdGF0ZSB2ZWxpdCBlc3NlIGNpbGx1bSBkb2xvcmUgZXUgZnVnaWF0IG51bGxhIHBhcmlhdHVyLiBFeGNlcHRldXIgc2ludCBvY2NhZWNhdCBjdXBpZGF0YXQgbm9uIHByb2lkZW50LCBzdW50IGluIGN1bHBhIHF1aSBvZmZpY2lhIGRlc2VydW50IG1vbGxpdCBhbmltIGlkIGVzdCBsYWJvcnVt\"",
        );
        // assert_lisp("(base64-encode-string \"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum\" t)", "\"TG9yZW0gaXBzdW0gZG9sb3Igc2l0IGFtZXQsIGNvbnNlY3RldHVyIGFkaXBpc2NpbmcgZWxpdCwg\nc2VkIGRvIGVpdXNtb2QgdGVtcG9yIGluY2lkaWR1bnQgdXQgbGFib3JlIGV0IGRvbG9yZSBtYWdu\nYSBhbGlxdWEuIFV0IGVuaW0gYWQgbWluaW0gdmVuaWFtLCBxdWlzIG5vc3RydWQgZXhlcmNpdGF0\naW9uIHVsbGFtY28gbGFib3JpcyBuaXNpIHV0IGFsaXF1aXAgZXggZWEgY29tbW9kbyBjb25zZXF1\nYXQuIER1aXMgYXV0ZSBpcnVyZSBkb2xvciBpbiByZXByZWhlbmRlcml0IGluIHZvbHVwdGF0ZSB2\nZWxpdCBlc3NlIGNpbGx1bSBkb2xvcmUgZXUgZnVnaWF0IG51bGxhIHBhcmlhdHVyLiBFeGNlcHRl\ndXIgc2ludCBvY2NhZWNhdCBjdXBpZGF0YXQgbm9uIHByb2lkZW50LCBzdW50IGluIGN1bHBhIHF1\naSBvZmZpY2lhIGRlc2VydW50IG1vbGxpdCBhbmltIGlkIGVzdCBsYWJvcnVt\"");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_base64_encode_string_prop() {
        proptest! {|(string in arb_custom_string("[\x00-\x7F]*"))| {
            assert_elprop![r#"(base64-encode-string {} t)"#, string];
        }};
    }

    #[test]
    fn rassoc_compares_cdrs_structurally_and_returns_first_original_entry() {
        assert_lisp("(rassoc 2 '((a . 1) (b . 2) (c . 2)))", "(b . 2)");
        assert_lisp("(rassoc '(1 2) '((a 1 2) (b 1 2)))", "(a 1 2)");
        assert_lisp("(rassoc \"é\" (list nil 99 (cons 'a (concat \"é\"))))", "(a . \"é\")");
        assert_lisp("(rassoc nil '(ignored (a . nil) (b . nil)))", "(a)");
        assert_lisp("(rassoc 9 '((a . 1) nil ignored))", "nil");
        assert_lisp("(rassoc 1 nil)", "nil");
        assert_lisp(
            "(let ((entry (cons 'a (list 1 2)))) (eq (rassoc (list 1 2) (list entry)) entry))",
            "t",
        );
    }

    #[test]
    fn rassoc_rejects_malformed_outer_alists_without_panicking() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let dotted = crate::core::cons::Cons::new(1i64, 2i64, cx);
        assert!(super::rassoc(42i64.into(), dotted.into()).is_err());
        let circular = crate::core::cons::Cons::new(1i64, NIL, cx);
        circular.set_cdr(circular.into()).unwrap();
        assert!(super::rassoc(42i64.into(), circular.into()).is_err());
    }

    #[test]
    fn copy_sequence_records_are_shallow_and_independent() {
        assert_lisp(
            "(let* ((item (list 1)) (source (record 'tag item 2)) (copy (copy-sequence source))) (list (eq source copy) (eq (aref source 1) (aref copy 1)) (aref copy 0)))",
            "(nil t tag)",
        );
        assert_lisp(
            "(let* ((source (record 'tag 1 2)) (copy (copy-sequence source))) (aset copy 1 9) (list (aref source 1) (aref copy 1) (type-of copy)))",
            "(1 9 tag)",
        );
        assert_lisp("(aref (copy-sequence (record 'tag)) 0)", "tag");
    }

    #[test]
    fn copy_sequence_preserves_raw_bytes_and_storage_kind() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let bytes = cx.add(vec![0u8, 128, 255]);
        let copy = super::copy_sequence(bytes, cx).unwrap();
        assert!(matches!(copy.untag(), crate::core::object::ObjectType::ByteString(_)));
        assert!(!super::eq(bytes, copy));
        assert_eq!(bytes, copy);
        let unicode = cx.add("é🦀");
        let unicode_copy = super::copy_sequence(unicode, cx).unwrap();
        assert!(matches!(unicode_copy.untag(), crate::core::object::ObjectType::String(_)));
        assert!(!super::eq(unicode, unicode_copy));
        assert_eq!(unicode, unicode_copy);
        assert!(super::copy_sequence(42i64.into(), cx).is_err());
    }

    #[test]
    fn safe_length_counts_prefixed_cycles_without_underreporting() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let cells: Vec<_> = (0..8)
            .map(|value| crate::core::cons::Cons::new(value as i64, NIL, cx))
            .collect();
        for pair in cells.windows(2) {
            pair[0].set_cdr(pair[1].into()).unwrap();
        }
        // Prefix length5 followed by cycle length3: old Floyd count was6,
        // below the8 distinct cells required by GNU's safe-length contract.
        cells[7].set_cdr(cells[5].into()).unwrap();
        assert!(super::safe_length(cells[0].into()) >= cells.len());
        let singleton = crate::core::cons::Cons::new(1i64, NIL, cx);
        singleton.set_cdr(singleton.into()).unwrap();
        assert!(super::safe_length(singleton.into()) >= 1);
        assert_eq!(super::safe_length(42i64.into()), 0);
        assert_eq!(super::safe_length(NIL), 0);
        let dotted = crate::core::cons::Cons::new(1i64, 2i64, cx);
        assert_eq!(super::safe_length(dotted.into()), 1);
    }

    #[test]
    fn length_comparisons_cover_sequences_and_short_circuit_lists() {
        assert_lisp("(list (length= nil 0) (length< nil 1) (length> nil -1))", "(t t t)");
        assert_lisp("(list (length= [1 2] 2) (length< [1 2] 2) (length> [1 2] 1))", "(t nil t)");
        assert_lisp(
            "(list (length= \"é🦀\" 2) (length< \"é🦀\" 3) (length> \"é🦀\" 2))",
            "(t t nil)",
        );
        assert_lisp("(list (length= '(1 2) 2) (length< '(1 2) 3) (length> '(1 2) 2))", "(t t nil)");
        assert_lisp(
            "(list (length= '(1 . 2) 1) (length< '(1 . 2) 2) (length> '(1 . 2) 0))",
            "(t t t)",
        );
        assert_lisp("(length= 42 -1)", "nil");
        assert_lisp(
            "(let ((p (list 1))) (setcdr p p) (list (length= p 3) (length< p 3) (length> p 3)))",
            "(nil nil t)",
        );
    }

    #[test]
    fn length_comparisons_check_large_bounds_and_invalid_sequences() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let bytes = cx.add(vec![0u8, 128, 255]);
        assert!(super::length_equal(bytes, 3).unwrap());
        assert!(super::length_less(bytes, i64::MAX).unwrap());
        assert!(super::length_greater(bytes, i64::MIN).unwrap());
        assert!(super::length_equal(42i64.into(), 0).is_err());
        assert!(super::length_less(42i64.into(), -1).is_err());
        assert!(super::length_greater(42i64.into(), -1).is_err());
        let circular = crate::core::cons::Cons::new(1i64, NIL, cx);
        circular.set_cdr(circular.into()).unwrap();
        assert!(super::length_equal(circular.into(), i64::MAX).is_err());
    }

    #[test]
    fn featurep_observes_provide_and_subfeature_properties() {
        assert_lisp("(featurep 'roadmap-feature-absent)", "nil");
        assert_lisp(
            "(progn (provide 'roadmap-feature-present) (list (featurep 'roadmap-feature-present) (featurep 'roadmap-feature-present nil) (featurep 'roadmap-feature-present 'missing)))",
            "(t t nil)",
        );
        assert_lisp(
            "(progn (provide 'roadmap-feature-with-subfeatures) (put 'roadmap-feature-with-subfeatures 'subfeatures (list 'one (list 1 2))) (list (featurep 'roadmap-feature-with-subfeatures 'one) (featurep 'roadmap-feature-with-subfeatures (list 1 2)) (featurep 'roadmap-feature-with-subfeatures 'two)))",
            "(t t nil)",
        );
        assert_lisp(
            "(progn (put 'roadmap-feature-unprovided 'subfeatures '(one)) (featurep 'roadmap-feature-unprovided 'one))",
            "nil",
        );
    }

    #[test]
    fn plist_put_updates_first_pair_and_preserves_identity() {
        assert_lisp("(plist-put nil 'a 1)", "(a 1)");
        assert_lisp("(plist-put (list 'a 1) 'b 2)", "(a 1 b 2)");
        assert_lisp("(plist-put (list 'a 1 'a 2) 'a 3)", "(a 3 a 2)");
        assert_lisp("(let ((p (list 'a 1))) (eq p (plist-put p 'a nil)))", "t");
        assert_lisp("(let ((p (list 'a 1))) (eq p (plist-put p 'b 2 nil)))", "t");
        assert_lisp("(let ((key (list 1))) (plist-put (list (list 1) 2) key 3))", "((1) 2 (1) 3)");
        assert_lisp("(plist-put (list 'a 1 'odd) 'a 2)", "(a 2 odd)");
    }

    #[test]
    fn plist_put_rejects_malformed_lists_and_unsupported_predicate() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let key = cx.add(1i64);
        let value = cx.add(2i64);
        assert!(super::plist_put(value, key, value, None, cx).is_err());
        let odd = crate::core::cons::Cons::new(key, NIL, cx);
        assert!(super::plist_put(odd.into(), key, value, None, cx).is_err());
        let dotted = crate::core::cons::Cons::new(key, value, cx);
        assert!(super::plist_put(dotted.into(), key, value, None, cx).is_err());
        let circular = crate::core::cons::Cons::new(key, NIL, cx);
        circular.set_cdr(circular.into()).unwrap();
        assert!(super::plist_put(circular.into(), value, key, None, cx).is_err());
        assert!(super::plist_put(NIL, key, value, Some(value), cx).is_err());
    }

    #[test]
    fn concat_character_sequences() {
        assert_lisp("(concat)", "\"\"");
        assert_lisp("(concat nil [] '(65 233) [129408] \"z\")", "\"Aé🦀z\"");
    }

    #[test]
    fn mapconcat_character_results_and_separators() {
        assert_lisp("(mapconcat 'identity '((65) [233] \"🦀\") [45])", "\"A-é-🦀\"");
        assert_lisp("(mapconcat 'identity '(nil (65) nil) '(45))", "\"-A-\"");
        assert_lisp("(mapconcat 'identity '((65) (66)))", "\"AB\"");
        assert_lisp("(mapconcat 'identity nil 42)", "\"\"");
        assert_lisp("(mapconcat 'identity '((65)) 42)", "\"A\"");
    }

    #[test]
    fn concat_preserves_control_characters() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let sequence = super::slice_into_list(&[0i64.into(), 10i64.into(), 9i64.into()], None, cx);
        assert_eq!(super::concat(&[sequence]).unwrap(), "\0\n\t");
    }

    #[test]
    fn concat_rejects_invalid_character_sequences() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        for character in [-1i64, 0xd800, 0x110000, i64::MAX] {
            let sequence = crate::core::cons::Cons::new(character, NIL, cx);
            assert!(super::concat(&[sequence.into()]).is_err());
        }
        let dotted = crate::core::cons::Cons::new(65i64, 66i64, cx);
        assert!(super::concat(&[dotted.into()]).is_err());
        assert!(super::concat(&[42i64.into()]).is_err());
        let noncharacter = crate::core::cons::Cons::new(true, NIL, cx);
        assert!(super::concat(&[noncharacter.into()]).is_err());
    }

    #[test]
    fn mapping_array_sequences() {
        assert_lisp("(mapcar 'identity [1 2 3])", "(1 2 3)");
        assert_lisp("(mapcar 'identity \"é🦀\")", "(233 129408)");
        assert_lisp("(mapcar 'identity [])", "nil");
        assert_lisp("(mapcar 'identity \"\")", "nil");
        assert_lisp("(let ((v [1 2])) (eq (mapc 'identity v) v))", "t");
        assert_lisp("(let ((s \"é🦀\")) (eq (mapc 'identity s) s))", "t");
        assert_lisp(
            "(let ((result nil)) (mapc #'(lambda (x) (setq result (cons x result))) [1 2 3]) result)",
            "(3 2 1)",
        );
        assert_lisp("(mapcan #'(lambda (x) (list x x)) [1 2])", "(1 1 2 2)");
    }

    #[test]
    fn test_take() {
        assert_lisp("(take 2 '(1 2 3 4))", "(1 2)");
    }

    #[test]
    fn test_delq() {
        assert_lisp("(delq 1 '(1 2 3 1 4 1))", "(2 3 4)");
        assert_lisp("(delq t '(t t t))", "nil");
    }

    #[test]
    fn nth_and_nthcdr_negative_and_dotted_bounds() {
        assert_lisp("(nth -1 '(1 2))", "1");
        assert_lisp("(nthcdr -1 '(1 2))", "(1 2)");
        assert_lisp("(nthcdr 100 nil)", "nil");
    }

    #[test]
    fn nth_and_nthcdr_reject_past_dotted_tail() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let dotted = crate::core::cons::Cons::new(1i64, 2i64, cx);
        assert!(super::nthcdr(2, dotted.into()).is_err());
        assert!(super::nth(1, dotted.into()).is_err());
    }

    #[test]
    fn test_nthcdr() {
        assert_lisp("(nthcdr 0 nil)", "nil");
        assert_lisp("(nthcdr 3 nil)", "nil");
        assert_lisp("(nthcdr 0 '(1 2 3))", "(1 2 3)");
        assert_lisp("(nthcdr 1 '(1 2 3))", "(2 3)");
        assert_lisp("(nthcdr 2 '(1 2 3))", "(3)");
        assert_lisp("(nthcdr 3 '(1 2 3))", "nil");
        assert_lisp("(nthcdr 1 '(1 . 2))", "2");
        assert_lisp("(nthcdr 2 '(1 2 . 3))", "3");
    }

    #[test]
    fn reverse_array_sequences_and_vector_identity() {
        assert_lisp("(reverse [1 2 3])", "[3 2 1]");
        assert_lisp("(reverse [])", "[]");
        assert_lisp("(reverse \"aé🦀\")", "\"🦀éa\"");
        assert_lisp("(let ((v [1 2 3])) (list (reverse v) v))", "([3 2 1] [1 2 3])");
        assert_lisp("(let ((x (list 1))) (eq (aref (reverse (vector x)) 0) x))", "t");
        assert_lisp("(let ((v [1 2 3 4])) (list (eq (nreverse v) v) v))", "(t [4 3 2 1])");
        assert_lisp("(nreverse [])", "[]");
        assert_lisp("(nreverse [1])", "[1]");
    }

    #[test]
    fn reverse_unibyte_strings_and_invalid_sequences() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        let ascii = super::reverse(cx.add("ab"), cx).unwrap();
        assert!(matches!(ascii.untag(), crate::core::object::ObjectType::ByteString(_)));
        assert_eq!(ascii, cx.add(vec![b'b', b'a']));
        let bytes = cx.add(vec![0u8, 128, 255]);
        let reversed = super::reverse(bytes, cx).unwrap();
        assert!(matches!(reversed.untag(), crate::core::object::ObjectType::ByteString(_)));
        assert_eq!(reversed, cx.add(vec![255u8, 128, 0]));
        assert_eq!(bytes, cx.add(vec![0u8, 128, 255]));
        assert!(super::reverse(1i64.into(), cx).is_err());
        assert!(super::nreverse(1i64.into()).is_err());
        let dotted = crate::core::cons::Cons::new(1i64, 2i64, cx);
        assert!(super::reverse(dotted.into(), cx).is_err());
    }

    #[test]
    fn test_reverse() {
        assert_lisp("(nreverse nil)", "nil");
        assert_lisp("(nreverse '(1))", "(1)");
        assert_lisp("(nreverse '(1 2))", "(2 1)");
        assert_lisp("(nreverse '(1 2 3))", "(3 2 1)");
        assert_lisp("(nreverse '(1 2 3 4))", "(4 3 2 1)");
    }

    #[test]
    fn nconc_arbitrary_tail_and_dotted_prefix() {
        assert_lisp("(nconc)", "nil");
        assert_lisp("(nconc 42)", "42");
        assert_lisp("(nconc nil 42)", "42");
        assert_lisp("(nconc (list 1 2) 3)", "(1 2 . 3)");
        assert_lisp("(nconc (cons 1 2) (list 3))", "(1 3)");
        assert_lisp("(nconc (list 1) nil (list 2) nil)", "(1 2)");
        assert_lisp("(let ((tail (list 2))) (eq (cdr (nconc (list 1) tail)) tail))", "t");
    }

    #[test]
    fn nconc_rejects_nonfinal_atom() {
        assert!(super::nconc(&[42i64.into(), NIL]).is_err());
    }

    #[test]
    fn test_nconc() {
        assert_lisp("(nconc nil)", "nil");
        assert_lisp("(nconc '(1 2))", "(1 2)");
        assert_lisp("(nconc '(1 2) '(3 4))", "(1 2 3 4)");
        assert_lisp("(nconc '(1 2) '(3 4) '(5 6))", "(1 2 3 4 5 6)");
        assert_lisp("(nconc nil '(1 2))", "(1 2)");
        assert_lisp("(nconc '(1 2) nil)", "(1 2)");
    }

    #[test]
    fn append_preserves_final_tail() {
        assert_lisp("(append)", "nil");
        assert_lisp("(append 42)", "42");
        assert_lisp("(append '(1 2) 3)", "(1 2 . 3)");
        assert_lisp("(let ((tail (list 2 3))) (eq (cdr (append '(1) tail)) tail))", "t");
        assert_lisp("(let ((tail (list 2 3))) (eq (append tail) tail))", "t");
        assert_lisp("(append [1 2] '(3))", "(1 2 3)");
        assert_lisp("(append \"é🦀\" nil)", "(233 129408)");
    }

    #[test]
    fn append_rejects_invalid_nonfinal_sequences() {
        let roots = &crate::core::gc::RootSet::default();
        let cx = &crate::core::gc::Context::new(roots);
        assert!(super::append(&[42i64.into(), NIL], cx).is_err());
        let dotted = crate::core::cons::Cons::new(1i64, 2i64, cx);
        assert!(super::append(&[dotted.into(), NIL], cx).is_err());
    }

    #[test]
    fn test_append() {
        assert_lisp("(append \"hello\" nil)", "(104 101 108 108 111)");
    }

    #[test]
    fn test_assq() {
        assert_lisp("(assq 5 '((1 . 2) (3 . 4) (5 . 6)))", "(5 . 6)");
        assert_lisp("(assq 6 '((1 . 2) (3 . 4) (5 . 6)))", "nil");
    }

    #[test]
    fn test_string_equal() {
        assert_lisp("(string-equal \"hello\" \"hello\")", "t");
        assert_lisp("(string-equal \"hello\" \"world\")", "nil");
    }

    #[test]
    fn test_compare_strings() {
        assert_lisp("(compare-strings \"hello\" 0 6 \"hello\" 0 6)", "t");
        assert_lisp("(compare-strings \"hello\" 0 6 \"world\" 0 6)", "-1");
        assert_lisp("(compare-strings \"hello\" 0 6 \"HELLO\" 0 6 t)", "t");
    }

    #[test]
    fn test_string_distance() {
        assert_lisp("(string-distance \"hello\" \"hello\")", "0");
        assert_lisp("(string-distance \"hello\" \"jello\")", "1");
        assert_lisp("(string-distance \"hello\" \"world\")", "4");
    }

    #[test]
    fn test_levenstein_distance() {
        assert_eq!(4, levenshtein_distance("hello".chars(), "world".chars()));
        assert_eq!(3, levenshtein_distance("kitten".chars(), "sitting".chars()));
        assert_eq!(2, levenshtein_distance("book".chars(), "back".chars()));
        assert_eq!(5, levenshtein_distance("table".chars(), "dinner".chars()));
        assert_eq!(2, levenshtein_distance("person".chars(), "pardon".chars()));
        assert_eq!(1, levenshtein_distance("person".chars(), "persons".chars()));
        assert_eq!(4, levenshtein_distance("book".chars(), "".chars()));
        assert_eq!(4, levenshtein_distance("".chars(), "book".chars()));
        assert_eq!(0, levenshtein_distance("".chars(), "".chars()));
        assert_eq!(2, levenshtein_distance("Späße".chars(), "Spaß".chars()));
        assert_eq!(5, levenshtein_distance("さようなら".chars(), "こんにちは".chars()));
        assert_eq!(1, levenshtein_distance("さようなら".chars(), "さようなう".chars()));
        assert_eq!(4, levenshtein_distance("こんにちは".chars(), "こんにちは abc".chars()));
        assert_eq!(1, levenshtein_distance("༆༃ʘ".chars(), "༆˥ʘ".chars()));
    }

    #[test]
    fn test_string_lessp() {
        // String Tests
        // Test Equality
        assert_lisp("(string-lessp \"less\" \"less\")", "nil");
        // Test Length
        assert_lisp("(string-lessp \"les\" \"less\")", "t");
        assert_lisp("(string-lessp \"less\" \"les\")", "nil");
        // Check for less than
        assert_lisp("(string-lessp \"abc\" \"bcd\")", "t");
        assert_lisp("(string-lessp \"abx\" \"abcd\")", "nil");

        // Symbol Tests
        // Test Equality
        assert_lisp("(string-lessp 'less 'less)", "nil");
        // Test Length
        assert_lisp("(string-lessp 'les 'less)", "t");
        assert_lisp("(string-lessp 'less 'les)", "nil");
        // Check for less than
        assert_lisp("(string-lessp 'abc 'bcd)", "t");

        // Mixed Tests
        assert_lisp("(string-lessp 'less \"less\")", "nil");
        assert_lisp("(string-lessp 'les \"less\")", "t");
    }

    #[test]
    #[cfg(not(miri))] // slow
    fn test_string_version_lessp() {
        // String Tests
        // Test Equality
        assert_lisp("(string-version-lessp \"less\" \"less\")", "nil");
        // Test Length
        assert_lisp("(string-version-lessp \"les\" \"less\")", "t");
        assert_lisp("(string-version-lessp \"less\" \"les\")", "nil");
        // Test for less than
        assert_lisp("(string-version-lessp \"abc\" \"bcd\")", "t");
        // Test Equality with number
        assert_lisp("(string-version-lessp \"less1\" \"less1\")", "nil");
        assert_lisp("(string-version-lessp \"100a\" \"100b\")", "t");
        // Test Differing numeric position does matter
        assert_lisp("(string-version-lessp \"1less\" \"less1\")", "t");
        // Test Greater than numericaly
        assert_lisp("(string-version-lessp \"less12\" \"less1\")", "nil");
        // Test Less than numericaly
        assert_lisp("(string-version-lessp \"less1\" \"less12\")", "t");
        // Test that later numbers override previous ones
        assert_lisp("(string-version-lessp \"133less1\" \"less12\")", "t");
        // Test that digits don't disappear
        assert_lisp("(string-version-lessp \"112a\" \"512a\")", "t");
        // Test Numbers take higher precedence over characters
        assert_lisp("(string-version-lessp \"101a\" \"100b\")", "nil");
        // Test Numbers that leading zeros are ignored
        assert_lisp("(string-version-lessp \"10\" \"0100\")", "t");
        // Test that that the first number is compared immediately at a char comparison
        assert_lisp("(string-version-lessp \"10a100\" \"0100a\")", "t");
        assert_lisp("(string-version-lessp \"a100\" \"0100a\")", "nil");
        assert_lisp("(string-version-lessp \"01\" \"1\")", "nil");
        assert_lisp("(string-version-lessp \"a1a10aa\" \"a1a0100\")", "t");
        assert_lisp("(string-version-lessp \"a1b2\" \"a1a3\")", "nil");
        assert_lisp("(string-version-lessp \".\" \".\")", "nil");
        assert_lisp("(string-version-lessp \"..\" \"..\")", "nil");
        assert_lisp("(string-version-lessp \"..\" \".a\")", "t");
        assert_lisp("(string-version-lessp \"101a\" \"100b\")", "nil");
        assert_lisp("(string-version-lessp \"10\" \"0100\")", "t");
        assert_lisp("(string-version-lessp \"001\" \"01\")", "nil");
        assert_lisp("(string-version-lessp \"a1a10\" \"a1a0100\")", "t");
        assert_lisp("(string-version-lessp \"A\" \":\")", "t");
        assert_lisp("(string-version-lessp \"\" \"\")", "nil");
        assert_lisp("(string-version-lessp \"\" \".\")", "t");
        assert_lisp("(string-version-lessp \".\" \".\")", "nil");
        assert_lisp("(string-version-lessp \".\" \"~\")", "t");
        assert_lisp("(string-version-lessp \"~\" \"a\")", "t");

        // Symbol Tests
        // Test Equality
        assert_lisp("(string-version-lessp 'less 'less)", "nil");
        // Test Length
        assert_lisp("(string-version-lessp 'les 'less)", "t");
        assert_lisp("(string-version-lessp 'less 'les)", "nil");
        // Check for less than
        assert_lisp("(string-version-lessp 'abc 'bcd)", "t");
        // Test Equality with number
        assert_lisp("(string-version-lessp 'less1 'less1)", "nil");
        assert_lisp("(string-version-lessp '100a '100b)", "t");
        // Test Differing numeric position does matter
        assert_lisp("(string-version-lessp '1less 'less1)", "t");
        // Test Greater than numericaly
        assert_lisp("(string-version-lessp 'less12 'less1)", "nil");
        // Test Less than numericaly
        assert_lisp("(string-version-lessp 'less1 'less12)", "t");
        // Test that later numbers override previous ones
        assert_lisp("(string-version-lessp '133less1 'less12)", "t");
        // Test Numbers take higher precedence over characters
        assert_lisp("(string-version-lessp '101a '100b)", "nil");
        // Test Numbers that leading zeros are ignored
        // Test that that the first number is compared immediately at a char comparison
        assert_lisp("(string-version-lessp '10a100 '0100a)", "t");
        assert_lisp("(string-version-lessp 'a100 '0100a)", "nil");

        // Mixed Tests
        assert_lisp("(string-version-lessp 'less \"less\")", "nil");
        assert_lisp("(string-version-lessp 'less1 \"less10\")", "t");
    }

    #[test]
    fn test_base64url_encode_string() {
        assert_lisp("(base64url-encode-string \"hello\")", "\"aGVsbG8=\"");
        assert_lisp("(base64url-encode-string \"hello\" nil)", "\"aGVsbG8=\"");
        assert_lisp("(base64url-encode-string \"hello\" t)", "\"aGVsbG8\"");
        assert_lisp("(base64url-encode-string \"hello\" 0)", "\"aGVsbG8\"");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_base64url_encode_string_prop() {
        proptest! {|(string in arb_custom_string("[\x00-\x7F]*"))| {
            assert_elprop![r#"(base64url-encode-string {} t)"#, string];
        }};
    }

    #[test]
    #[cfg(miri)]
    fn test_maphash() {
        // This test does not assert anything, but allows this to be checked by
        // miri
        assert_lisp(
            "(let ((h (make-hash-table))) (puthash 1 6 h) (puthash 2 8 h) (puthash 3 10 h) (maphash 'eq h))",
            "nil",
        );
    }

    #[test]
    fn test_legnth() {
        assert_lisp("(length nil)", "0");
        assert_lisp("(length '(1 2 3))", "3");
        assert_lisp("(length \"hello\")", "5");
        assert_lisp("(length [1 2 3])", "3");
        assert_lisp("(safe-length '(1 . 2))", "1");
        assert_lisp("(safe-length '(1 2 3 . 4))", "3");
        assert_lisp("(safe-length 'foo)", "0");
    }

    #[test]
    fn test_sort() {
        assert_lisp("(sort nil '<)", "nil");
        assert_lisp("(sort '(1) '<)", "(1)");
        assert_lisp("(sort '(2 1) '<)", "(1 2)");
        assert_lisp("(sort '(1 2 3) '<)", "(1 2 3)");
        assert_lisp("(sort '(3 2 1) '<)", "(1 2 3)");
        assert_lisp("(sort '(3 1 2) '<)", "(1 2 3)");
        assert_lisp("(sort '(1 2 3 4 5) '>)", "(5 4 3 2 1)");
        assert_lisp(
            "(sort '((1 . 1) (1 . 2) (1 . 3)) 'car-less-than-car)",
            "((1 . 1) (1 . 2) (1 . 3))",
        );
        assert_lisp("(condition-case nil (sort '(3 2 1) 'length) (error 7))", "7");
    }

    #[test]
    fn test_copy_alist() {
        assert_lisp("(copy-alist '((1 . 2) (3 . 4) (5 . 6)))", "((1 . 2) (3 . 4) (5 . 6))");
    }

    #[test]
    fn test_clear_string() {
        assert_lisp(
            "(let ((str \"test string\")) (clear-string str) str)",
            "\"\0\0\0\0\0\0\0\0\0\0\0\"",
        );
        assert_lisp("(let ((str \"\")) (clear-string str) str)", "\"\"");
    }
}

#[cfg(test)]
mod binary_string_protocol_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn actual_lisp_binary_accumulator_and_cookie_slice_preserve_representation() -> Result<()> {
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        crate::sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        crate::core::env::init_variables(cx, env);
        let packet = crate::core::env::intern("binary-protocol-packet", cx);
        env.set_var(packet, cx.add(vec![0xfe, b'3', 0xff, 0xce, 0xbb, b'!']))?;
        let source = r#"(let* ((joined (concat "" binary-protocol-packet)) (payload (substring joined 3 -1))) (list (multibyte-string-p joined) (aref joined 0) (aref joined 2) (length joined) (multibyte-string-p payload) (aref payload 0) (aref payload 1)))"#;
        let form = crate::reader::read(source, cx)?.0; root!(form, cx);
        let result = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
        assert_eq!(result.to_string(), "(nil 254 255 6 nil 206 187)");
        Ok(())
    }

    #[test]
    fn concat_and_substring_keep_protocol_bytes_and_use_byte_positions() -> Result<()> {
        let roots = RootSet::default(); let cx = Context::new(&roots);
        let empty = cx.add(""); let packet = cx.add(vec![0xfe, b'3', 0xff, 0xce, 0xbb, b'!']);
        let joined = concat_lisp(&[empty, packet], &cx)?;
        let ObjectType::ByteString(bytes) = joined.untag() else { panic!("raw protocol bytes became Unicode") };
        assert_eq!(&**bytes, &[0xfe,b'3',0xff,0xce,0xbb,b'!']);
        let payload = substring_lisp(joined, Some(3), Some(-1), &cx)?;
        let ObjectType::ByteString(bytes) = payload.untag() else { panic!("substring lost byte representation") };
        assert_eq!(&**bytes, &[0xce,0xbb]);
        assert!(substring_lisp(joined, Some(-99), None, &cx).is_err());
        assert!(concat_lisp(&[packet, cx.add("λ")], &cx).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod string_property_transform_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn lisp_property_transforms_match_gnu() {
        crate::assert_elprop!(r#"(let* ((value (list 42)) (s (propertize "λa🙂" 'face value))
          (part (substring s 1)) (copied (copy-sequence s)) (joined (concat "x" s "y")))
          (list (equal part "a🙂") (eq (get-text-property 0 'face part) value)
                (eq s copied) (eq (get-text-property 1 'face copied) value)
                (get-text-property 0 'face joined) (eq (get-text-property 1 'face joined) value)
                (get-text-property 4 'face joined)
                (text-properties-at 0 (substring-no-properties s))))"#);
    }
    #[test]
    fn unicode_slice_concat_and_copy_preserve_spans_and_shallow_values() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let value: Object = list![42; cx];
        let key = crate::core::env::intern("rune-property-transform", cx);
        let plist = list![key, value; cx];
        let input = unsafe { copy_string_with_properties(cx.add("aλ🙂z"), &[StringPropertyRun {start:1,end:3,plist}], cx) }.unwrap();
        let slice = substring_lisp(input, Some(1), Some(-1), cx).unwrap();
        let runs = string_property_runs(slice).unwrap();
        assert_eq!((runs[0].start,runs[0].end),(0,2));
        assert!(!eq(runs[0].plist, plist));
        let values: Vec<_> = runs[0].plist.as_list().unwrap().into_iter().map(|v| v.unwrap()).collect();
        assert_eq!(values[1], value);
        let copied = copy_sequence(input,cx).unwrap();
        assert!(!eq(copied,input));
        assert_eq!(string_property_runs(copied).unwrap()[0].start,1);
        let joined = concat_lisp(&[cx.add("x"),input,cx.add("y")],cx).unwrap();
        let joined_runs = string_property_runs(joined).unwrap();
        assert_eq!((joined_runs[0].start,joined_runs[0].end),(2,4));
        let plain = substring_no_properties(input, Some(1),Some(3),cx).unwrap();
        assert!(string_property_runs(plain).unwrap().is_empty());
    }
    #[test]
    fn byte_slice_keeps_raw_octets_and_uses_byte_coordinates() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let plist = list![42, true; cx];
        let input = unsafe { copy_string_with_properties(cx.add(vec![0xfe,b'x',0xff]), &[StringPropertyRun{start:0,end:2,plist}],cx) }.unwrap();
        let slice = substring_lisp(input,Some(1),None,cx).unwrap();
        let ObjectType::ByteString(bytes) = slice.untag() else {panic!("byte kind lost")};
        assert_eq!(&**bytes,&[b'x',0xff]);
        let runs = string_property_runs(slice).unwrap();
        assert_eq!((runs[0].start,runs[0].end),(0,1));
    }
    #[test]
    fn property_bearing_insert_rejects_before_owner_edit() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        let plist = list![42, true; cx];
        let input = unsafe { copy_string_with_properties(cx.add("abc"), &[StringPropertyRun{start:0,end:3,plist}],cx) }.unwrap();
        env.stack.push(input);
        let before = env.buffer_read(cx).unwrap();
        assert!(crate::editfns::insert(crate::core::env::ArgSlice::new(1),env,cx).is_err());
        let after = env.buffer_read(cx).unwrap();
        assert_eq!(before.epoch,after.epoch);
        assert_eq!(before.point,after.point);
        assert_eq!(after.document.text().to_string(), "");
    }

}
