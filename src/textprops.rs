use crate::{
    core::{
        cons::Cons,
        env::Env,
        error::{Type, TypeError},
        gc::{Context, Rt, Slot},
        object::{NIL, Object, ObjectType, TRUE, WithLifetime},
    },
    fns::eq,
    intervals::textget,
};
use anyhow::{Result, anyhow, bail};
use rune_core::macros::list;
use rune_macros::defun;

use crate::core::object::BufferData;

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PropertySetType {
    Replace,
    Prepend,
    Append,
}

/// Add the properties of PLIST to the interval I, or set
/// the value of I's property to the value of the property on PLIST
/// if they are different.
///
/// OBJECT should be the string or buffer the interval is in.
///
/// If DESTRUCTIVE, the function is allowed to reuse list values in the
/// properties.
///
/// Return true if this changes I (i.e., if any members of PLIST
/// are actually added to I's plist)
pub fn add_properties<'ob>(
    plist: Object<'ob>,
    obj_i: Object<'ob>,
    _set_type: PropertySetType,
    _destructive: bool,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    merge_properties(plist, obj_i, cx).map(|(properties, _)| properties)
}

/// Return the final plist and whether any sequential incoming assignment
/// changed a value/presence, even if a later duplicate restores the old value.
fn merge_properties<'ob>(
    plist: Object<'ob>,
    mut obj_i: Object<'ob>,
    cx: &'ob Context,
) -> Result<(Object<'ob>, bool)> {
    // Property lists are shared by split intervals and may also belong to Lisp.
    // Build a fresh list instead of changing any existing cons cells.
    let mut pairs = Vec::new();
    let mut existing = obj_i.as_list()?;
    while let Some(key) = existing.next() {
        let key = key?;
        let value = existing.next().ok_or_else(|| anyhow!("Odd length property list"))??;
        pairs.push((key, value));
    }
    let original_pairs = pairs.clone();
    let mut changed = false;
    let mut incoming = plist.as_list()?;
    while let Some(key) = incoming.next() {
        let key = key?;
        let value = incoming.next().ok_or_else(|| anyhow!("Odd length property list"))??;
        if let Some(pair) = pairs.iter_mut().find(|pair| eq(pair.0, key)) {
            changed |= !eq(pair.1, value);
            pair.1 = value;
        } else {
            changed = true;
            pairs.push((key, value));
        }
    }
    if pairs.len() == original_pairs.len()
        && pairs.iter().zip(&original_pairs).all(|(a, b)| eq(a.0, b.0) && eq(a.1, b.1))
    {
        return Ok((obj_i, changed));
    }
    obj_i = NIL;
    for (key, value) in pairs.into_iter().rev() {
        obj_i = Cons::new(key, Cons::new(value, obj_i, cx), cx).into();
    }
    Ok((obj_i, changed))
}

/// Helper function to safely modify buffer data.
///
/// Takes an object (buffer or nil) and environment, and applies the given function
/// to the buffer's data. Handles both current buffer and other buffers safely.
///
/// # Arguments
/// * `object` - The buffer object to modify (or nil for current buffer)
/// * `env` - The environment containing buffer state
/// * `func` - Function to apply to the buffer's data
///
/// # Returns
/// Result containing the return value from func or an error
fn modify_buffer_data<'ob, T>(
    object: Object<'ob>,
    env: &'ob mut Rt<Env>,
    func: impl FnOnce(&mut BufferData) -> Result<T>,
) -> Result<T> {
    if object.is_nil() {
        let data = env.current_buffer.get_mut();
        func(data)
    } else {
        let current_buf = env.current_buffer.buf_ref;
        if let Some(b) = object.buffer() {
            if b == current_buf {
                let data = env.current_buffer.get_mut();
                func(data)
            } else {
                let mut open_buf = b.lock()?;
                func(open_buf.get_mut())
            }
        } else {
            Err(anyhow!(TypeError::new(Type::BufferOrString, object.untag())))
        }
    }
}

/// GNU validate_interval_range treats empty mutation ranges as no-ops before
/// checking bounds; nonempty buffer ranges use one-based character positions.
fn validate_property_mutation_range(data: &BufferData, start: usize, end: usize) -> Result<bool> {
    if start == end {
        return Ok(false);
    }
    let point_max = data.text.len_chars() + 1;
    if start == 0 || end > point_max {
        bail!("Buffer range {start}..{end} is out of range 1..={point_max}");
    }
    Ok(true)
}

/// Return the list of properties of the character at POSITION in OBJECT.
/// If the optional second argument OBJECT is a buffer (or nil, which means
/// the current buffer), POSITION is a buffer position (integer or marker).
///
/// If OBJECT is a string, POSITION is a 0-based index into it.
///
/// If POSITION is at the end of OBJECT, the value is nil, but note that
/// buffer narrowing does not affect the value.  That is, if OBJECT is a
/// buffer or nil, and the buffer is narrowed and POSITION is at the end
/// of the narrowed buffer, the result may be non-nil.
///
/// If you want to display the text properties at point in a human-readable
/// form, use the `describe-text-properties' command.
#[defun]
pub fn text_properties_at<'ob>(
    position: usize,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    if matches!(object.untag(), ObjectType::String(_)) {
        bail!("String text properties are not supported yet");
    }
    modify_buffer_data(object, env, |data| {
        let point_max = data.text.len_chars() + 1;
        if position == 0 || position > point_max {
            bail!("Buffer position {position} is out of range 1..={point_max}");
        }
        // The endpoint has no character, even if an internal caller has
        // installed a property interval extending beyond the buffer text.
        if position == point_max {
            return Ok(NIL);
        }
        let mut tree = data.textprops.lock(cx)?;
        let properties = tree.find(position).map(|node| *node.val).unwrap_or(NIL);
        // The returned Lisp object belongs to the GC context, not to the
        // temporary buffer mutex guard used to look it up.
        Ok(unsafe { properties.with_lifetime() })
    })
}

/// Return the value of POSITION's property PROP, in OBJECT.
/// OBJECT should be a buffer or a string; if omitted or nil, it defaults
/// to the current buffer.
///
/// If POSITION is at the end of OBJECT, the value is nil, but note that
/// buffer narrowing does not affect the value.  That is, if the buffer is
/// narrowed and POSITION is at the end of the narrowed buffer, the result
/// may be non-nil.
#[defun]
pub fn get_text_property<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let props = text_properties_at(position, object, env, &cx)?;
    // TODO see lookup_char_property, should also lookup
    // 1. category
    // 2. char_property_alias_alist
    // 3.  default_text_properties
    textget(props, prop)
}

/// Return (VALUE . OVERLAY). Rune currently has no overlays/windows, so the
/// supported buffer case follows GNU's text-property fallback with a nil cdr.
#[defun]
pub fn get_char_property_and_overlay<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let value = get_text_property(position, prop, object, env, &cx)?;
    Ok(Cons::new(value, NIL, cx).into())
}

/// Read a character property from supported buffer text. Overlay/window
/// precedence and string properties require their respective data models.
#[defun]
pub fn get_char_property<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    get_text_property(position, prop, object, env, &cx)
}

// Keep direct Rust callers safe even when passing an i64 outside the tagged
// fixnum range. Lisp-facing LIMIT values currently enter through fixnums.
fn property_scan_limit_object<'ob>(limit: i64, cx: &'ob Context) -> Object<'ob> {
    use crate::core::object::IntoObject;
    crate::arith::NumberValue::Big(limit.into())
        .coerce_integer()
        .into_obj(cx)
        .into()
}

/// Return the next character-property boundary, or point-max if none exists.
/// This is the text-only fallback until Rune has an overlay data model.
#[defun]
pub fn next_char_property_change<'ob>(
    position: usize,
    limit: Option<i64>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let point_max = env.current_buffer.get().text.len_chars() + 1;
    if position == 0 || position > point_max {
        bail!("Buffer position {position} is out of range 1..={point_max}");
    }
    let point_max = i64::try_from(point_max)?;
    let limit = limit.unwrap_or(point_max).min(point_max);
    if limit <= i64::try_from(position)? {
        return Ok(property_scan_limit_object(limit, cx));
    }
    next_property_change(position, None, Some(property_scan_limit_object(limit, cx)), env, cx)
}

/// Return the previous character-property boundary, or point-min if none exists.
/// Overlay boundaries will be included when the overlay data model is added.
#[defun]
pub fn previous_char_property_change<'ob>(
    position: usize,
    limit: Option<i64>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let point_max = env.current_buffer.get().text.len_chars() + 1;
    if position == 0 || position > point_max {
        bail!("Buffer position {position} is out of range 1..={point_max}");
    }
    let limit = limit.unwrap_or(1).max(1);
    if limit >= i64::try_from(position)? {
        return Ok(property_scan_limit_object(limit, cx));
    }
    previous_property_change(position, None, Some(usize::try_from(limit)?), env, cx)
}

/// Return the next boundary of PROP or the buffer endpoint, ignoring changes
/// to unrelated properties. This implements the no-overlay buffer case.
#[defun]
pub fn next_single_char_property_change<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    limit: Option<i64>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let point_max = character_property_scan_point_max(position, object, env)?;
    let point_max = i64::try_from(point_max)?;
    let limit = limit.unwrap_or(point_max).min(point_max);
    if limit <= i64::try_from(position)? {
        return Ok(property_scan_limit_object(limit, cx));
    }
    next_single_property_change(position, prop, object, Some(usize::try_from(limit)?), env, cx)
}

/// Return the previous boundary of PROP or point-min. Overlay and string
/// support remain dependent on those data models, as for the other fallbacks.
#[defun]
pub fn previous_single_char_property_change<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    limit: Option<i64>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    character_property_scan_point_max(position, object, env)?;
    let limit = limit.unwrap_or(1).max(1);
    if limit >= i64::try_from(position)? {
        return Ok(property_scan_limit_object(limit, cx));
    }
    previous_single_property_change(position, prop, object, Some(usize::try_from(limit)?), env, cx)
}

fn character_property_scan_point_max(
    position: usize,
    object: Option<Object<'_>>,
    env: &mut Rt<Env>,
) -> Result<usize> {
    modify_buffer_data(object.unwrap_or(NIL), env, |data| {
        let point_max = data.text.len_chars() + 1;
        if position == 0 || position > point_max {
            bail!("Buffer position {position} is out of range 1..={point_max}");
        }
        Ok(point_max)
    })
}

/// Add or replace properties on text from START to END, preserving unrelated
/// properties. Return t if any property actually changes, nil otherwise.
/// OBJECT defaults to the current buffer.
#[defun]
pub fn add_text_properties<'ob>(
    start: usize,
    end: usize,
    properties: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<bool> {
    // GNU validates first and treats an empty plist as a no-op without even
    // examining OBJECT or bounds (textprop.c add_text_properties_1).
    let properties = copy_text_property_plist(properties, cx)?;
    if properties.is_nil() {
        return Ok(false);
    }
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| {
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(false);
        }
        let mut tree = data.textprops.lock(cx)?;
        let mut changed = false;
        // Normalize duplicate keys for uncovered gaps, where insertion stores
        // the incoming plist directly. GNU applies later assignments last.
        let normalized = merge_properties(properties, NIL, cx)?.0;
        // Include gaps: adding a nil-valued property is still a change in
        // property presence, unlike merely looking up its effective value.
        // Validate every affected plist before making any tree changes.
        for (_, existing) in tree.iter(start, end) {
            let (_, modified) = merge_properties(properties, existing, cx)?;
            changed |= modified;
        }
        if changed {
            tree.insert(start, end, Slot::new(normalized), cx)?;
        }
        Ok(changed)
    })
}

/// Set one property of the text from START to END.
/// The third and fourth arguments PROPERTY and VALUE
/// specify the property to add.
/// If the optional fifth argument OBJECT is a buffer (or nil, which means
/// the current buffer), START and END are buffer positions (integers or
/// markers).  If OBJECT is a string, START and END are 0-based indices into it.
#[defun]
pub fn put_text_property<'ob>(
    start: usize,
    end: usize,
    property: Object<'ob>,
    value: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<()> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    let prop = list!(property, value; cx);
    modify_buffer_data(object, env, |data| {
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(());
        }
        let mut tree = data.textprops.lock(cx)?;
        tree.insert(start, end, Slot::new(prop), cx)
    })
}
/// Return the position of next property change.
/// Scans characters forward from POSITION in OBJECT till it finds
/// a change in some text property, then returns the position of the change.
/// If the optional second argument OBJECT is a buffer (or nil, which means
/// the current buffer), POSITION is a buffer position (integer or marker).
/// If OBJECT is a string, POSITION is a 0-based index into it.
/// Return nil if LIMIT is nil or omitted, and the property is constant all
/// the way to the end of OBJECT; if the value is non-nil, it is a position
/// greater than POSITION, never equal.
///
/// If the optional third argument LIMIT is non-nil, don't search
/// past position LIMIT; return LIMIT if nothing is found before LIMIT.
#[defun]
pub fn next_property_change<'ob>(
    position: usize,
    object: Option<Object<'ob>>,
    limit: Option<Object<'ob>>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    let limit = limit.filter(|limit| !limit.is_nil());
    let next_interval = limit.is_some_and(|limit| eq(limit, TRUE));
    let limit: Option<usize> =
        if next_interval { None } else { limit.map(TryInto::try_into).transpose()? };
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_max = data.text.len_chars() + 1;
        if next_interval {
            if position == 0 || position > point_max {
                bail!("Buffer position {position} is out of range 1..={point_max}");
            }
            let mut tree = data.textprops.lock(cx)?;
            let next = tree.tree.find_intersect_min(position..point_max);
            let boundary = match next {
                Some(node) if node.key.start > position => node.key.start,
                Some(node) => node.key.end.min(point_max),
                None => point_max,
            };
            return Ok(cx.add(boundary));
        }
        let end = limit.unwrap_or(point_max);
        let mut tree = data.textprops.lock(cx)?;
        let prop = tree.tree.find_intersect_min(position..end);

        match prop {
            Some(p) => {
                let range = p.key;
                if range.start <= position {
                    // should have range.end > position
                    if range.end < end { Ok(cx.add(range.end)) } else { Ok(cx.add(limit)) }
                } else {
                    // empty property before interval, so prop changed just at range start
                    Ok(cx.add(range.start))
                }

                // range is fully inside region, prop changes at end
            }
            None => {
                let result = match limit {
                    Some(n) => cx.add(n),
                    None => NIL,
                };
                Ok(result)
            }
        }
    })
}

/// Return the position of next property change for a specific property.
/// Scans characters forward from POSITION till it finds
/// a change in the PROP property, then returns the position of the change.
/// If the optional third argument OBJECT is a buffer (or nil, which means
/// the current buffer), POSITION is a buffer position (integer or marker).
/// If OBJECT is a string, POSITION is a 0-based index into it.
/// The property values are compared with `eq'.
/// Return nil if LIMIT is nil or omitted, and the property is constant all
/// the way to the end of OBJECT; if the value is non-nil, it is a position
/// greater than POSITION, never equal.
///
/// If the optional fourth argument LIMIT is non-nil, don't search
/// past position LIMIT; return LIMIT if nothing is found before LIMIT.
#[defun]
pub fn next_single_property_change<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_max = data.text.len_chars() + 1;
        let end = limit.unwrap_or(point_max);
        let mut tree = data.textprops.lock(cx)?;
        let iter = tree.iter(position, end);

        let mut val = None;
        for (interval, props) in iter {
            let text_prop = textget(props, prop)?;
            match val {
                Some(v) => {
                    if eq(v, text_prop) {
                        continue;
                    } else {
                        return Ok(cx.add(interval.start));
                    }
                }
                None => val = Some(text_prop),
            }
        }
        Ok(cx.add(limit))
    })
}

/// Return the position of previous property change.
/// Scans characters backwards from POSITION in OBJECT till it finds
/// a change in some text property, then returns the position of the change.
/// If the optional second argument OBJECT is a buffer (or nil, which means
/// the current buffer), POSITION is a buffer position (integer or marker).
/// If OBJECT is a string, POSITION is a 0-based index into it.
/// Return nil if LIMIT is nil or omitted, and the property is constant all
/// the way to the start of OBJECT; if the value is non-nil, it is a position
/// less than POSITION, never equal.
///
/// If the optional third argument LIMIT is non-nil, don't search
/// back past position LIMIT; return LIMIT if nothing is found until LIMIT.
#[defun]
pub fn previous_property_change<'ob>(
    position: usize,
    object: Option<Object<'ob>>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_min = 1;
        let start = limit.unwrap_or(point_min);
        let end = position;
        let mut tree = data.textprops.lock(cx)?;
        let prop = tree.tree.find_intersect_max(start..end);

        match prop {
            Some(p) => {
                let interval = p.key;
                if interval.end >= position {
                    // should have range.start < position
                    if interval.start > start {
                        Ok(cx.add(interval.start))
                    } else {
                        Ok(cx.add(limit))
                    }
                } else {
                    // empty property after interval, so prop changed just at its end
                    Ok(cx.add(interval.end))
                }

                // range is fully inside region, prop changes at end
            }
            None => {
                let result = match limit {
                    Some(n) => cx.add(n),
                    None => NIL,
                };
                Ok(result)
            }
        }
    })
}

/// Return the position of next property change for a specific property.
/// Scans characters forward from POSITION till it finds
/// a change in the PROP property, then returns the position of the change.
/// If the optional third argument OBJECT is a buffer (or nil, which means
/// the current buffer), POSITION is a buffer position (integer or marker).
/// If OBJECT is a string, POSITION is a 0-based index into it.
/// The property values are compared with `eq'.
/// Return nil if LIMIT is nil or omitted, and the property is constant all
/// the way to the end of OBJECT; if the value is non-nil, it is a position
/// greater than POSITION, never equal.
///
/// If the optional fourth argument LIMIT is non-nil, don't search
/// past position LIMIT; return LIMIT if nothing is found before LIMIT.
#[defun]
pub fn previous_single_property_change<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Option<Object<'ob>>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_min = 1;
        let start = limit.unwrap_or(point_min);
        let mut tree = data.textprops.lock(cx)?;
        let iter = tree.iter_reverse(start, position);

        let mut val = None;
        for (interval, props) in iter {
            let text_prop = textget(props, prop)?;
            match val {
                Some(v) => {
                    if eq(v, text_prop) {
                        continue;
                    } else {
                        // Walking backwards, the differing segment meets the
                        // previously visited segment at its exclusive end.
                        return Ok(cx.add(interval.end));
                    }
                }
                None => val = Some(text_prop),
            }
        }
        Ok(cx.add(limit))
    })
}

/// Validate and shallow-copy the plist spine before storing it in a buffer.
/// Property keys/values retain Lisp identity, but caller cons mutations must not
/// silently change installed text properties. GNU also accepts an atom as a
/// shorthand for a single property with a nil value.
fn copy_text_property_plist<'ob>(properties: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    if properties.is_nil() {
        return Ok(NIL);
    }
    if properties.cons().is_none() {
        return Ok(list![properties, NIL; cx]);
    }
    let mut pairs = Vec::new();
    let mut properties = properties.as_list()?;
    while let Some(key) = properties.next() {
        let key = key?;
        let value = properties.next().ok_or_else(|| anyhow!("Odd length text property list"))??;
        pairs.push((key, value));
    }
    let mut result = NIL;
    for (key, value) in pairs.into_iter().rev() {
        result = Cons::new(key, Cons::new(value, result, cx), cx).into();
    }
    Ok(result)
}

/// Completely replace properties of text from START to END.
/// The third argument PROPERTIES is the new property list.
/// If the optional fourth argument OBJECT is a buffer (or nil, which means
/// the current buffer), START and END are buffer positions (integers or
/// markers).  If OBJECT is a string, START and END are 0-based indices into it.
/// If PROPERTIES is nil, the effect is to remove all properties from
/// the designated part of OBJECT.
#[defun]
pub fn set_text_properties<'ob>(
    start: usize,
    end: usize,
    properties: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<()> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<()> {
        let properties = copy_text_property_plist(properties, cx)?;
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(());
        }
        let mut tree = data.textprops.lock(cx)?;
        tree.set_properties(start, end, properties);
        Ok(())
    })
}

/// Remove some properties from text from START to END.
/// The third argument PROPERTIES is a property list
/// whose property names specify the properties to remove.
/// \(The values stored in PROPERTIES are ignored.)
/// If the optional fourth argument OBJECT is a buffer (or nil, which means
/// the current buffer), START and END are buffer positions (integers or
/// markers).  If OBJECT is a string, START and END are 0-based indices into it.
/// Return t if any property was actually removed, nil otherwise.
///
/// Use `set-text-properties' if you want to remove all text properties.
#[defun]
pub fn remove_text_properties<'ob>(
    start: usize,
    end: usize,
    properties: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<bool> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<bool> {
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(false);
        }
        let mut tree = data.textprops.lock(cx)?;
        // GNU textprop.c traverses candidate keys without validate_plist:
        // non-lists are no-ops, and a final key need not have a value.
        if !properties.is_nil() && properties.cons().is_none() {
            return Ok(false);
        }
        let mut keys = Vec::new();
        let mut properties = properties.as_list()?;
        while let Some(key) = properties.next() {
            keys.push(key?);
            if let Some(value) = properties.next() {
                value?;
            }
        }
        let mut list = NIL;
        for key in keys.into_iter().rev() {
            list = Cons::new(key, list, cx).into();
        }
        tree.delete(start, end, list, cx)
    })
}

/// /* Remove some properties from text from START to END.
/// The third argument LIST-OF-PROPERTIES is a list of property names to remove.
/// If the optional fourth argument OBJECT is a buffer (or nil, which means
/// the current buffer), START and END are buffer positions (integers or
/// markers).  If OBJECT is a string, START and END are 0-based indices into it.
/// Return t if any property was actually removed, nil otherwise.
#[defun]
pub fn remove_list_of_text_properties<'ob>(
    start: usize,
    end: usize,
    list_of_properties: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<bool> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<bool> {
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(false);
        }
        let mut tree = data.textprops.lock(cx)?;
        tree.delete(start, end, list_of_properties, cx)
    })
}

/// Check text from START to END for property PROPERTY equaling VALUE.
/// If so, return the position of the first character whose property PROPERTY
/// is `eq' to VALUE.  Otherwise return nil.
/// If the optional fifth argument OBJECT is a buffer (or nil, which means
/// the current buffer), START and END are buffer positions (integers or
/// markers).  If OBJECT is a string, START and END are 0-based indices into it.
#[defun]
pub fn text_property_any<'ob>(
    start: usize,
    end: usize,
    property: Object<'ob>,
    value: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let mut tree = data.textprops.lock(cx)?;
        let iter = tree.iter(start, end);
        for (interval, props) in iter {
            let val = textget(props, property)?;
            if eq(val, value) {
                return Ok(cx.add(interval.start));
            }
        }
        Ok(NIL)
    })
}

/// Check text from START to END for property PROPERTY not equaling VALUE.
/// If so, return the position of the first character whose property PROPERTY
/// is not `eq' to VALUE.  Otherwise, return nil.
/// If the optional fifth argument OBJECT is a buffer (or nil, which means
/// the current buffer), START and END are buffer positions (integers or
/// markers).  If OBJECT is a string, START and END are 0-based indices into it.
#[defun]
pub fn text_property_not_all<'ob>(
    start: usize,
    end: usize,
    property: Object<'ob>,
    value: Object<'ob>,
    object: Option<Object<'ob>>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let mut tree = data.textprops.lock(cx)?;
        let iter = tree.iter(start, end);
        for (interval, props) in iter {
            let val = textget(props, property)?;
            if !eq(val, value) {
                return Ok(cx.add(interval.start));
            }
        }
        Ok(NIL)
    })
}

#[cfg(test)]
mod tests {
    use crate::{
        buffer::{BUFFERS, get_buffer_create},
        core::{
            env::intern,
            gc::{Context, RootSet},
        },
        fns::plist_get,
    };
    use rune_core::macros::{list, root};

    use super::*;

    #[test]
    fn roadmap_m0_add_properties_does_not_change_shared_list() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let a = intern(":roadmap-a", &cx);
        let b = intern(":roadmap-b", &cx);
        let original = list![a, 1; &cx];
        let additions = list![a, 2, b, 3; &cx];
        let result =
            add_properties(additions, original, PropertySetType::Append, false, &cx).unwrap();
        assert_eq!(plist_get(original, a.into()).unwrap(), 1);
        assert_eq!(plist_get(result, a.into()).unwrap(), 2);
        assert_eq!(plist_get(result, b.into()).unwrap(), 3);
        assert_eq!(plist_get(additions, a.into()).unwrap(), 2);
    }

    #[test]
    fn roadmap_m0_property_noops_and_restore_coalesce() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-property-noops"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdefg"), &cx)?;
        let key: Object = intern("roadmap-face", &cx).into();
        let missing: Object = intern("roadmap-missing", &cx).into();
        let value = cx.add("bold");
        put_text_property(1, 7, key, value, Some(buf), env, &cx)?;
        put_text_property(2, 4, key, value, Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, Some(buf), None, env, &cx)?, 7);
        remove_list_of_text_properties(2, 4, list![missing; &cx], Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, Some(buf), None, env, &cx)?, 7);
        // Equal-looking strings are not eq and must retain a boundary.
        put_text_property(2, 4, key, cx.add("bold"), Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, Some(buf), None, env, &cx)?, 2);
        put_text_property(2, 4, key, value, Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, Some(buf), None, env, &cx)?, 7);
        Ok(())
    }

    #[test]
    fn test_add_properties() {
        let roots = &RootSet::default();
        let mut context = Context::new(roots);
        let cx = &mut context;
        let plist_1 = list![intern(":a", cx), 1, intern(":b", cx), 2; cx];
        let plist_2 = list![intern(":a", cx), 4, intern(":c", cx), 5; cx];
        let plist_1 =
            add_properties(plist_2, plist_1, PropertySetType::Replace, false, cx).unwrap();
        let plist_1 = dbg!(plist_1);
        let a = plist_get(plist_1, intern(":a", cx).into()).unwrap();
        let b = plist_get(plist_1, intern(":b", cx).into()).unwrap();
        let c = plist_get(plist_1, intern(":c", cx).into()).unwrap();
        assert_eq!(a, 4);
        assert_eq!(b, 2);
        assert_eq!(c, 5);
    }

    #[test]
    #[cfg(not(miri))]
    fn test_next_property_change() -> Result<()> {
        let roots = &RootSet::default();
        let mut context = Context::new(roots);
        let cx = &mut context;
        root!(env, new(Env), cx);

        let buf = get_buffer_create(cx.add("test_next_property_change"), None, cx)?;
        if let Some(b) = buf.buffer() {
            b.lock()
                .unwrap()
                .get_mut()
                .text
                .insert("lorem ipsum quia dolor sit amet, consectetur, adipisci velit.");
        }

        let a = intern(":a", cx);
        let a = cx.add(a);

        // Add properties at different ranges
        put_text_property(1, 5, a, cx.add(1), Some(buf), env, cx)?;
        put_text_property(5, 10, a, cx.add(2), Some(buf), env, cx)?;
        put_text_property(15, 20, a, cx.add(3), Some(buf), env, cx)?;

        // Test property changes
        let change = next_property_change(1, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(5)); // Change at end of first range

        let change = next_property_change(5, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(10)); // Change at end of second range

        let change = next_property_change(10, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(15)); // Change at start of third range

        let change = next_property_change(15, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(20)); // Change at end of third range

        // Test with limit
        let change = next_property_change(1, Some(buf), Some(cx.add(8)), env, cx)?;
        assert_eq!(change, cx.add(5)); // Change within limit

        let change = next_property_change(5, Some(buf), Some(cx.add(8)), env, cx)?;
        assert_eq!(change, cx.add(8)); // Limit reached

        // Test no change case
        let change = next_property_change(20, Some(buf), None, env, cx)?;
        assert!(change.is_nil()); // No changes after last property

        BUFFERS.lock().unwrap().clear();
        Ok(())
    }

    #[test]
    #[cfg(not(miri))]
    fn test_next_single_property_change() -> Result<()> {
        let roots = &RootSet::default();
        let mut context = Context::new(roots);
        let cx = &mut context;
        root!(env, new(Env), cx);

        let buf = get_buffer_create(cx.add("test_next_single_property_change"), None, cx)?;
        if let ObjectType::Buffer(b) = buf.untag() {
            b.lock()
                .unwrap()
                .get_mut()
                .text
                .insert("lorem ipsum quia dolor sit amet, consectetur, adipisci velit.");
        }

        let a = intern(":a", cx);
        let a = cx.add(a);
        let b = intern(":b", cx);
        let b = cx.add(b);

        // Add properties at different ranges
        put_text_property(1, 5, a, cx.add(1), Some(buf), env, cx)?;
        put_text_property(5, 8, a, cx.add(2), Some(buf), env, cx)?;
        put_text_property(10, 15, b, cx.add(4), Some(buf), env, cx)?;
        put_text_property(15, 20, a, cx.add(3), Some(buf), env, cx)?;

        // Test property changes for :a
        let change = next_single_property_change(1, a, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(5)); // Change at end of first range

        let change = next_single_property_change(5, a, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(8)); // Change at end of second range

        let change = next_single_property_change(10, a, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(15)); // Change at start of third range

        let change = next_single_property_change(15, a, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(20)); // Change at end of third range

        // Test with limit
        let change = next_single_property_change(1, a, Some(buf), Some(8), env, cx)?;
        assert_eq!(change, cx.add(5)); // Change within limit

        let change = next_single_property_change(5, a, Some(buf), Some(8), env, cx)?;
        assert_eq!(change, cx.add(8)); // Limit reached

        // Test no change case
        let change = next_single_property_change(20, a, Some(buf), None, env, cx)?;
        assert!(change.is_nil()); // No changes after last property

        // Test property changes for :b
        let change = next_single_property_change(1, b, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(10)); // Change at start of b range

        let change = next_single_property_change(10, b, Some(buf), None, env, cx)?;
        assert_eq!(change, cx.add(15)); // Change at end of b range

        BUFFERS.lock().unwrap().clear();
        Ok(())
    }

    #[test]
    fn roadmap_m0_remove_candidates_match_gnu_traversal() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-remove-candidates"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcde"), &cx)?;
        let a: Object = intern("roadmap-remove-a", &cx).into();
        let b: Object = intern("roadmap-remove-b", &cx).into();
        put_text_property(1, 5, a, cx.add(1), Some(buf), env, &cx)?;
        put_text_property(1, 5, b, cx.add(2), Some(buf), env, &cx)?;
        // GNU's CONSP candidate loop ignores a bare symbol, rather than
        // interpreting it as a property name or rejecting it.
        remove_text_properties(2, 4, a, Some(buf), env, &cx)?;
        assert_eq!(plist_get(text_properties_at(2, Some(buf), env, &cx)?, a)?, 1);
        assert_eq!(next_property_change(1, Some(buf), None, env, &cx)?, 5);
        // A final proper-list key is still a removal candidate with no value.
        remove_text_properties(2, 4, list![a; &cx], Some(buf), env, &cx)?;
        assert!(plist_get(text_properties_at(2, Some(buf), env, &cx)?, a)?.is_nil());
        assert_eq!(plist_get(text_properties_at(1, Some(buf), env, &cx)?, a)?, 1);
        assert_eq!(plist_get(text_properties_at(4, Some(buf), env, &cx)?, a)?, 1);
        assert_eq!(plist_get(text_properties_at(2, Some(buf), env, &cx)?, b)?, 2);
        Ok(())
    }

    #[test]
    fn roadmap_m0_previous_single_change_reports_shared_boundary() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-previous-single"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdefghij"), &cx)?;
        let key: Object = intern("roadmap-previous-key", &cx).into();
        let other: Object = intern("roadmap-previous-other", &cx).into();
        let value = cx.add("value");
        put_text_property(2, 5, key, value, Some(buf), env, &cx)?;
        put_text_property(5, 8, key, cx.add("value"), Some(buf), env, &cx)?;
        // Equal-looking but non-eq strings have a real boundary at 5.
        assert_eq!(previous_single_property_change(7, key, Some(buf), None, env, &cx)?, 5);
        assert_eq!(previous_single_property_change(5, key, Some(buf), None, env, &cx)?, 2);
        // A gap after a property range changes at the range's end, not start.
        assert_eq!(previous_single_property_change(10, key, Some(buf), None, env, &cx)?, 8);
        // Unrelated boundaries do not interrupt the specific property scan.
        put_text_property(3, 4, other, cx.add(1), Some(buf), env, &cx)?;
        assert_eq!(previous_single_property_change(5, key, Some(buf), None, env, &cx)?, 2);
        assert_eq!(previous_single_property_change(7, key, Some(buf), Some(6), env, &cx)?, 6);
        assert!(previous_single_property_change(2, key, Some(buf), None, env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_reversed_property_mutations_normalize_ranges() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-reversed-properties"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        let a: Object = intern("roadmap-reversed-a", &cx).into();
        let b: Object = intern("roadmap-reversed-b", &cx).into();
        put_text_property(6, 2, a, cx.add(1), Some(buf), env, &cx)?;
        assert!(get_text_property(1, a, Some(buf), env, &cx)?.is_nil());
        assert_eq!(get_text_property(2, a, Some(buf), env, &cx)?, 1);
        assert_eq!(get_text_property(5, a, Some(buf), env, &cx)?, 1);
        assert!(get_text_property(6, a, Some(buf), env, &cx)?.is_nil());
        set_text_properties(5, 3, list![b, 2; &cx], Some(buf), env, &cx)?;
        assert!(get_text_property(3, a, Some(buf), env, &cx)?.is_nil());
        assert_eq!(get_text_property(3, b, Some(buf), env, &cx)?, 2);
        assert_eq!(get_text_property(2, a, Some(buf), env, &cx)?, 1);
        assert_eq!(get_text_property(5, a, Some(buf), env, &cx)?, 1);
        remove_text_properties(4, 3, list![b, NIL; &cx], Some(buf), env, &cx)?;
        assert!(text_properties_at(3, Some(buf), env, &cx)?.is_nil());
        assert_eq!(get_text_property(4, b, Some(buf), env, &cx)?, 2);
        remove_list_of_text_properties(6, 4, list![a, b; &cx], Some(buf), env, &cx)?;
        assert!(text_properties_at(4, Some(buf), env, &cx)?.is_nil());
        assert!(text_properties_at(5, Some(buf), env, &cx)?.is_nil());
        assert_eq!(get_text_property(2, a, Some(buf), env, &cx)?, 1);
        // Normalization must leave empty ranges as no-ops.
        put_text_property(2, 2, a, cx.add(9), Some(buf), env, &cx)?;
        set_text_properties(2, 2, NIL, Some(buf), env, &cx)?;
        remove_text_properties(2, 2, list![a, NIL; &cx], Some(buf), env, &cx)?;
        remove_list_of_text_properties(2, 2, list![a; &cx], Some(buf), env, &cx)?;
        assert_eq!(get_text_property(2, a, Some(buf), env, &cx)?, 1);
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_search_predicates_and_identity() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-property-search"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        let key: Object = intern("roadmap-search-key", &cx).into();
        let value = cx.add("same");
        put_text_property(2, 5, key, value, Some(buf), env, &cx)?;
        assert_eq!(text_property_any(1, 7, key, value, Some(buf), env, &cx)?, 2);
        assert_eq!(text_property_any(3, 7, key, value, Some(buf), env, &cx)?, 3);
        assert!(text_property_any(1, 7, key, cx.add("same"), Some(buf), env, &cx)?.is_nil());
        assert_eq!(text_property_not_all(1, 7, key, value, Some(buf), env, &cx)?, 1);
        assert_eq!(text_property_not_all(2, 7, key, value, Some(buf), env, &cx)?, 5);
        assert!(text_property_not_all(2, 5, key, value, Some(buf), env, &cx)?.is_nil());
        assert_eq!(text_property_any(2, 7, key, NIL, Some(buf), env, &cx)?, 5);
        assert_eq!(text_property_not_all(1, 7, key, NIL, Some(buf), env, &cx)?, 2);
        assert_eq!(text_property_any(7, 1, key, value, Some(buf), env, &cx)?, 2);
        assert_eq!(text_property_not_all(7, 2, key, value, Some(buf), env, &cx)?, 5);
        assert!(text_property_any(3, 3, key, value, Some(buf), env, &cx)?.is_nil());
        assert!(text_property_not_all(3, 3, key, NIL, Some(buf), env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_removal_reports_actual_changes() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-removal-results"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        let key: Object = intern("roadmap-removal-result-key", &cx).into();
        let missing: Object = intern("roadmap-removal-result-missing", &cx).into();
        put_text_property(1, 6, key, NIL, Some(buf), env, &cx)?;
        assert!(!remove_text_properties(2, 4, key, Some(buf), env, &cx)?);
        assert!(!remove_text_properties(2, 4, list![missing, 1; &cx], Some(buf), env, &cx)?);
        assert!(!remove_list_of_text_properties(2, 2, list![key; &cx], Some(buf), env, &cx)?);
        // Presence, not a non-nil value, determines whether removal changed text.
        assert!(remove_text_properties(2, 4, list![key, 999; &cx], Some(buf), env, &cx)?);
        assert!(!remove_text_properties(2, 4, list![key, NIL; &cx], Some(buf), env, &cx)?);
        assert!(!text_properties_at(1, Some(buf), env, &cx)?.is_nil());
        assert!(text_properties_at(2, Some(buf), env, &cx)?.is_nil());
        assert!(!text_properties_at(4, Some(buf), env, &cx)?.is_nil());
        assert!(remove_list_of_text_properties(
            1,
            6,
            list![missing, key; &cx],
            Some(buf),
            env,
            &cx
        )?);
        assert!(!remove_list_of_text_properties(1, 6, list![key; &cx], Some(buf), env, &cx)?);
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_removal_returns_lisp_booleans() {
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (put-text-property 1 3 'a nil nil) (remove-text-properties 1 3 '(a ignored) nil))",
            "t",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (remove-text-properties 1 3 '(a ignored) nil))",
            "nil",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (put-text-property 1 3 'a nil nil) (remove-list-of-text-properties 1 3 '(a) nil))",
            "t",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (remove-list-of-text-properties 1 3 '(a) nil))",
            "nil",
        );
    }

    #[test]
    fn roadmap_m0_optional_property_object_keeps_explicit_buffer_isolated() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        env.current_buffer.get_mut().insert(cx.add("abc"), &cx)?;
        let other = get_buffer_create(cx.add("roadmap-optional-other"), None, &cx)?;
        other.buffer().unwrap().lock()?.insert(cx.add("abc"), &cx)?;
        let key: Object = intern("roadmap-optional-key", &cx).into();
        put_text_property(1, 3, key, cx.add(1), None, env, &cx)?;
        set_text_properties(1, 3, list![key, 2; &cx], Some(other), env, &cx)?;
        assert_eq!(get_text_property(1, key, None, env, &cx)?, 1);
        assert_eq!(get_text_property(1, key, Some(other), env, &cx)?, 2);
        assert!(remove_text_properties(1, 3, list![key, NIL; &cx], None, env, &cx)?);
        assert_eq!(get_text_property(1, key, Some(other), env, &cx)?, 2);
        assert!(remove_list_of_text_properties(1, 3, list![key; &cx], Some(other), env, &cx)?);
        assert!(get_text_property(1, key, Some(other), env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_mutators_accept_omitted_object() {
        for (source, expected) in [
            (
                "(progn (insert \"abc\") (put-text-property 1 2 'a 7) (put-text-property 2 3 'a 7 nil) (list (get-text-property 1 'a nil) (get-text-property 2 'a nil)))",
                "(7 7)",
            ),
            (
                "(progn (insert \"abc\") (set-text-properties 1 2 '(a 8)) (set-text-properties 2 3 '(a 8) nil) (list (get-text-property 1 'a nil) (get-text-property 2 'a nil)))",
                "(8 8)",
            ),
            (
                "(progn (insert \"abc\") (put-text-property 1 3 'a nil) (list (remove-text-properties 1 2 '(a ignored)) (remove-text-properties 2 3 '(a ignored) nil)))",
                "(t t)",
            ),
            (
                "(progn (insert \"abc\") (put-text-property 1 3 'a nil) (list (remove-list-of-text-properties 1 2 '(a)) (remove-list-of-text-properties 2 3 '(a) nil)))",
                "(t t)",
            ),
        ] {
            crate::interpreter::assert_lisp(source, expected);
        }
    }

    #[test]
    fn roadmap_m0_property_mutations_validate_character_ranges_atomically() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-mutation-bounds"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("é🙂"), &cx)?;
        let key: Object = intern("roadmap-mutation-bounds-key", &cx).into();
        put_text_property(1, 3, key, cx.add(7), Some(buf), env, &cx)?;
        for (start, end) in [(0, 2), (1, 4), (4, 1), (1, usize::MAX)] {
            assert!(put_text_property(start, end, key, cx.add(8), Some(buf), env, &cx).is_err());
            assert!(set_text_properties(start, end, NIL, Some(buf), env, &cx).is_err());
            assert!(
                remove_text_properties(start, end, list![key, NIL; &cx], Some(buf), env, &cx)
                    .is_err()
            );
            assert!(
                remove_list_of_text_properties(start, end, list![key; &cx], Some(buf), env, &cx)
                    .is_err()
            );
            assert_eq!(get_text_property(1, key, Some(buf), env, &cx)?, 7);
            assert_eq!(get_text_property(2, key, Some(buf), env, &cx)?, 7);
            assert!(text_properties_at(3, Some(buf), env, &cx)?.is_nil());
        }
        // GNU returns without validating the bounds of an empty range.
        put_text_property(usize::MAX, usize::MAX, key, NIL, Some(buf), env, &cx)?;
        set_text_properties(0, 0, NIL, Some(buf), env, &cx)?;
        assert!(!remove_text_properties(0, 0, list![key; &cx], Some(buf), env, &cx)?);
        assert!(!remove_list_of_text_properties(
            usize::MAX,
            usize::MAX,
            list![key; &cx],
            Some(buf),
            env,
            &cx
        )?);
        assert_eq!(get_text_property(1, key, Some(buf), env, &cx)?, 7);
        // Endpoint3 is legal for two Unicode characters, regardless of bytes.
        assert!(remove_list_of_text_properties(3, 1, list![key; &cx], Some(buf), env, &cx)?);
        assert!(text_properties_at(1, Some(buf), env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_set_properties_copies_plist_spine_and_validates() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-set-plist-copy"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abc"), &cx)?;
        let key: Object = intern("roadmap-set-plist-key", &cx).into();
        let value = cx.add("shared-value");
        let original = list![key, value; &cx];
        set_text_properties(1, 3, original, Some(buf), env, &cx)?;
        assert!(!eq(text_properties_at(1, Some(buf), env, &cx)?, original));
        assert!(eq(get_text_property(1, key, Some(buf), env, &cx)?, value));
        original.cons().unwrap().cdr().cons().unwrap().set_car(cx.add(99))?;
        original.cons().unwrap().set_cdr(NIL)?;
        assert!(eq(get_text_property(1, key, Some(buf), env, &cx)?, value));
        // Odd input must fail before any existing interval changes, even for
        // an empty range: GNU validates the plist before range processing.
        let odd = list![key; &cx];
        assert!(set_text_properties(1, 3, odd, Some(buf), env, &cx).is_err());
        assert!(set_text_properties(2, 2, odd, Some(buf), env, &cx).is_err());
        assert!(eq(get_text_property(1, key, Some(buf), env, &cx)?, value));
        set_text_properties(1, 3, key, Some(buf), env, &cx)?;
        let installed = text_properties_at(1, Some(buf), env, &cx)?;
        assert_eq!(installed.to_string(), "(roadmap-set-plist-key nil)");
        Ok(())
    }

    #[test]
    fn roadmap_m0_set_properties_do_not_alias_lisp_input() {
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (let ((p (list 'a 7))) (set-text-properties 1 3 p) (setcar (cdr p) 9) (get-text-property 1 'a nil)))",
            "7",
        );
    }

    #[test]
    fn roadmap_m0_add_text_properties_merges_and_reports_eq_changes() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-add-properties"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        let a: Object = intern("roadmap-add-a", &cx).into();
        let b: Object = intern("roadmap-add-b", &cx).into();
        let value = cx.add("same");
        put_text_property(1, 7, b, cx.add(9), Some(buf), env, &cx)?;
        let incoming = list![a, value; &cx];
        assert!(add_text_properties(2, 5, incoming, Some(buf), env, &cx)?);
        assert!(!add_text_properties(3, 4, incoming, Some(buf), env, &cx)?);
        assert_eq!(next_property_change(2, Some(buf), None, env, &cx)?, 5);
        assert!(eq(get_text_property(3, a, Some(buf), env, &cx)?, value));
        assert_eq!(get_text_property(3, b, Some(buf), env, &cx)?, 9);
        incoming.cons().unwrap().cdr().cons().unwrap().set_car(cx.add(8))?;
        assert!(eq(get_text_property(3, a, Some(buf), env, &cx)?, value));
        assert!(add_text_properties(3, 4, list![a, cx.add("same"); &cx], Some(buf), env, &cx)?);
        assert!(add_text_properties(5, 2, list![a, value; &cx], Some(buf), env, &cx)?);
        assert_eq!(next_property_change(2, Some(buf), None, env, &cx)?, 5);
        assert!(!add_text_properties(2, 5, list![a, value; &cx], Some(buf), env, &cx)?);
        assert!(add_text_properties(1, 2, list![a, NIL; &cx], Some(buf), env, &cx)?);
        assert!(!add_text_properties(1, 2, list![a, NIL; &cx], Some(buf), env, &cx)?);
        assert!(!add_text_properties(3, 3, list![a, value; &cx], Some(buf), env, &cx)?);
        assert!(!add_text_properties(usize::MAX, 0, NIL, Some(cx.add(42)), env, &cx)?);
        assert!(add_text_properties(0, 2, list![a, 1; &cx], Some(buf), env, &cx).is_err());
        assert!(add_text_properties(2, 5, list![a; &cx], Some(buf), env, &cx).is_err());
        assert!(eq(get_text_property(3, a, Some(buf), env, &cx)?, value));
        assert_eq!(get_text_property(3, b, Some(buf), env, &cx)?, 9);
        Ok(())
    }

    #[test]
    fn roadmap_m0_add_text_properties_applies_duplicate_keys_sequentially() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-add-duplicate-keys"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        let key: Object = intern("roadmap-add-duplicate", &cx).into();
        // Initially uncovered text must end up with the last assignment.
        assert!(add_text_properties(2, 5, list![key, 1, key, 2; &cx], Some(buf), env, &cx)?);
        assert_eq!(get_text_property(3, key, Some(buf), env, &cx)?, 2);
        // Changing and then restoring the original value still reports t,
        // while final equal intervals must not acquire artificial boundaries.
        assert!(add_text_properties(3, 4, list![key, 1, key, 2; &cx], Some(buf), env, &cx)?);
        assert_eq!(get_text_property(3, key, Some(buf), env, &cx)?, 2);
        assert_eq!(next_property_change(2, Some(buf), None, env, &cx)?, 5);
        assert!(!add_text_properties(3, 4, list![key, 2, key, 2; &cx], Some(buf), env, &cx)?);
        // A range spanning both covered text and gaps applies the same final
        // value everywhere and preserves nil-valued property presence.
        assert!(add_text_properties(1, 6, list![key, 9, key, NIL; &cx], Some(buf), env, &cx)?);
        for position in 1..6 {
            assert_eq!(
                text_properties_at(position, Some(buf), env, &cx)?.to_string(),
                "(roadmap-add-duplicate nil)"
            );
        }
        Ok(())
    }

    #[test]
    fn roadmap_m0_add_text_properties_lisp_results_and_optional_object() {
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (list (add-text-properties 1 3 '(a 1 b nil)) (add-text-properties 1 3 '(a 1 b nil) nil) (get-text-property 1 'a nil)))",
            "(t nil 1)",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (list (add-text-properties 1 2 'a) (text-properties-at 1 nil)))",
            "(t (a nil))",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (list (add-text-properties 1 3 '(a 1 a 2)) (get-text-property 1 'a nil) (add-text-properties 1 3 '(a 1 a 2)) (get-text-property 1 'a nil)))",
            "(t 2 t 2)",
        );
    }

    #[test]
    fn roadmap_m0_property_queries_accept_omitted_object() {
        for (query, expected) in [
            ("(equal (text-properties-at 1) (text-properties-at 1 nil))", "t"),
            ("(list (get-text-property 1 'a) (get-text-property 1 'a nil))", "(7 7)"),
            ("(list (next-property-change 1) (next-property-change 1 nil))", "(3 3)"),
            (
                "(list (next-single-property-change 1 'a) (next-single-property-change 1 'a nil))",
                "(3 3)",
            ),
            ("(list (previous-property-change 6) (previous-property-change 6 nil))", "(3 3)"),
            (
                "(list (previous-single-property-change 6 'a) (previous-single-property-change 6 'a nil))",
                "(3 3)",
            ),
            ("(list (text-property-any 1 7 'a 7) (text-property-any 1 7 'a 7 nil))", "(1 1)"),
            (
                "(list (text-property-not-all 1 7 'a 7) (text-property-not-all 1 7 'a 7 nil))",
                "(3 3)",
            ),
            (
                "(list (next-property-change 1 nil 2) (next-single-property-change 1 'a nil 2) (previous-property-change 6 nil 4) (previous-single-property-change 6 'a nil 4))",
                "(2 2 4 4)",
            ),
        ] {
            let source =
                format!("(progn (insert \"abcdef\") (put-text-property 1 3 'a 7) {query})");
            crate::interpreter::assert_lisp(&source, expected);
        }
    }

    #[test]
    fn roadmap_m0_optional_property_queries_preserve_explicit_object() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        env.current_buffer.get_mut().insert(cx.add("abcdef"), &cx)?;
        let other = get_buffer_create(cx.add("roadmap-query-other-buffer"), None, &cx)?;
        other.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        let key: Object = intern("roadmap-query-object-key", &cx).into();
        put_text_property(1, 3, key, cx.add(7), None, env, &cx)?;
        put_text_property(2, 5, key, cx.add(9), Some(other), env, &cx)?;
        assert!(!text_properties_at(1, None, env, &cx)?.is_nil());
        assert!(text_properties_at(1, Some(other), env, &cx)?.is_nil());
        assert_eq!(get_text_property(2, key, None, env, &cx)?, 7);
        assert_eq!(get_text_property(2, key, Some(other), env, &cx)?, 9);
        assert_eq!(next_property_change(1, None, None, env, &cx)?, 3);
        assert_eq!(next_property_change(2, Some(other), None, env, &cx)?, 5);
        assert_eq!(next_single_property_change(1, key, None, None, env, &cx)?, 3);
        assert_eq!(next_single_property_change(2, key, Some(other), None, env, &cx)?, 5);
        assert_eq!(previous_property_change(6, None, None, env, &cx)?, 3);
        assert_eq!(previous_property_change(6, Some(other), None, env, &cx)?, 5);
        assert_eq!(previous_single_property_change(6, key, None, None, env, &cx)?, 3);
        assert_eq!(previous_single_property_change(6, key, Some(other), None, env, &cx)?, 5);
        assert_eq!(text_property_any(1, 7, key, cx.add(7), None, env, &cx)?, 1);
        assert_eq!(text_property_any(1, 7, key, cx.add(9), Some(other), env, &cx)?, 2);
        assert_eq!(text_property_not_all(1, 7, key, cx.add(7), None, env, &cx)?, 3);
        assert_eq!(text_property_not_all(1, 7, key, cx.add(9), Some(other), env, &cx)?, 1);
        Ok(())
    }

    #[test]
    fn roadmap_m0_next_property_change_true_limit_returns_interval_boundary() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-true-limit"), None, &cx)?;
        assert_eq!(next_property_change(1, Some(buf), Some(TRUE), env, &cx)?, 1);
        buf.buffer().unwrap().lock()?.insert(cx.add("é🙂abcd"), &cx)?;
        let key: Object = intern("roadmap-true-limit-key", &cx).into();
        // Unpropertized text still returns the endpoint with t, not nil.
        assert_eq!(next_property_change(1, Some(buf), Some(TRUE), env, &cx)?, 7);
        assert!(next_property_change(1, Some(buf), None, env, &cx)?.is_nil());
        put_text_property(2, 5, key, cx.add(1), Some(buf), env, &cx)?;
        for (position, expected) in [(1, 2), (2, 5), (4, 5), (5, 7), (7, 7)] {
            assert_eq!(next_property_change(position, Some(buf), Some(TRUE), env, &cx)?, expected);
        }
        assert!(next_property_change(0, Some(buf), Some(TRUE), env, &cx).is_err());
        assert!(next_property_change(8, Some(buf), Some(TRUE), env, &cx).is_err());
        assert!(next_property_change(1, Some(buf), Some(cx.add("bad limit")), env, &cx).is_err());
        assert_eq!(next_property_change(2, Some(buf), Some(cx.add(3)), env, &cx)?, 3);
        put_text_property(1, 7, key, cx.add(2), Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, Some(buf), Some(TRUE), env, &cx)?, 7);
        assert!(next_property_change(1, Some(buf), None, env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_next_property_change_true_limit_lisp() {
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (list (next-property-change 1 nil t) (next-property-change 1)))",
            "(4 nil)",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abcdef\") (put-text-property 2 5 'a 1) (list (next-property-change 1 nil t) (next-property-change 2 nil t) (next-property-change 5 nil t)))",
            "(2 5 7)",
        );
    }

    #[test]
    fn roadmap_m0_character_property_text_fallback_preserves_identity() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        env.current_buffer.get_mut().insert(cx.add("é🙂"), &cx)?;
        let key: Object = intern("roadmap-char-property-key", &cx).into();
        let value = cx.add("identity");
        put_text_property(1, 3, key, value, None, env, &cx)?;
        assert!(eq(get_char_property(2, key, None, env, &cx)?, value));
        let pair = get_char_property_and_overlay(1, key, None, env, &cx)?;
        assert!(eq(pair.cons().unwrap().car(), value));
        assert!(pair.cons().unwrap().cdr().is_nil());
        assert!(get_char_property(3, key, None, env, &cx)?.is_nil());
        assert_eq!(get_char_property_and_overlay(3, key, None, env, &cx)?.to_string(), "(nil)");
        assert!(get_char_property(0, key, None, env, &cx).is_err());
        assert!(get_char_property_and_overlay(4, key, None, env, &cx).is_err());
        let other = get_buffer_create(cx.add("roadmap-char-property-other"), None, &cx)?;
        other.buffer().unwrap().lock()?.insert(cx.add("x"), &cx)?;
        assert!(get_char_property(1, key, Some(other), env, &cx)?.is_nil());
        assert_eq!(
            get_char_property_and_overlay(1, key, Some(other), env, &cx)?.to_string(),
            "(nil)"
        );
        assert!(get_char_property(1, key, Some(cx.add(42)), env, &cx).is_err());
        assert!(get_char_property(0, key, Some(cx.add("string")), env, &cx).is_err());
        Ok(())
    }

    #[test]
    fn roadmap_m0_character_property_text_fallback_lisp_results() {
        crate::interpreter::assert_lisp(
            "(progn (insert \"abc\") (put-text-property 1 3 'a 7) (list (get-char-property 1 'a) (get-char-property 1 'a nil) (get-char-property-and-overlay 1 'a) (get-char-property-and-overlay 3 'a nil) (get-char-property 4 'a)))",
            "(7 7 (7) (nil) nil)",
        );
    }

    #[test]
    fn roadmap_m0_character_property_scans_return_endpoints_and_limits() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        assert_eq!(next_char_property_change(1, None, env, &cx)?, 1);
        assert_eq!(previous_char_property_change(1, None, env, &cx)?, 1);
        env.current_buffer.get_mut().insert(cx.add("é🙂abcd"), &cx)?;
        let key: Object = intern("roadmap-char-scan-key", &cx).into();
        put_text_property(2, 5, key, cx.add(1), None, env, &cx)?;
        for (position, expected) in [(1, 2), (2, 5), (5, 7), (7, 7)] {
            assert_eq!(next_char_property_change(position, None, env, &cx)?, expected);
        }
        for (position, expected) in [(7, 5), (5, 2), (2, 1), (1, 1)] {
            assert_eq!(previous_char_property_change(position, None, env, &cx)?, expected);
        }
        assert_eq!(next_char_property_change(2, Some(3), env, &cx)?, 3);
        assert_eq!(next_char_property_change(5, Some(99), env, &cx)?, 7);
        assert_eq!(previous_char_property_change(7, Some(6), env, &cx)?, 6);
        assert_eq!(previous_char_property_change(2, Some(0), env, &cx)?, 1);
        assert_eq!(previous_char_property_change(7, Some(-1), env, &cx)?, 5);
        assert_eq!(previous_char_property_change(7, Some(i64::MIN), env, &cx)?, 5);
        assert_eq!(next_char_property_change(2, Some(-1), env, &cx)?, -1);
        assert_eq!(
            next_char_property_change(2, Some(i64::MIN), env, &cx)?.to_string(),
            i64::MIN.to_string()
        );
        assert!(next_char_property_change(0, None, env, &cx).is_err());
        assert!(previous_char_property_change(8, None, env, &cx).is_err());
        put_text_property(1, 7, key, cx.add(2), None, env, &cx)?;
        assert_eq!(next_char_property_change(1, None, env, &cx)?, 7);
        assert_eq!(previous_char_property_change(7, None, env, &cx)?, 1);
        Ok(())
    }

    #[test]
    fn roadmap_m0_character_property_scans_lisp_endpoints() {
        crate::interpreter::assert_lisp(
            "(list (next-char-property-change 1) (previous-char-property-change 1))",
            "(1 1)",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abcdef\") (put-text-property 2 5 'a 1) (list (next-char-property-change 1) (previous-char-property-change 7) (next-char-property-change 2 3) (previous-char-property-change 7 6)))",
            "(2 5 3 6)",
        );
        crate::interpreter::assert_lisp(
            "(progn (insert \"abcdef\") (put-text-property 2 5 'a 1) (list (next-char-property-change 2 -1) (previous-char-property-change 7 -1)))",
            "(-1 5)",
        );
    }

    #[test]
    fn roadmap_m0_single_character_property_scans_ignore_other_keys() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        assert_eq!(next_single_char_property_change(1, cx.add(1), None, None, env, &cx)?, 1);
        assert_eq!(previous_single_char_property_change(1, cx.add(1), None, None, env, &cx)?, 1);
        env.current_buffer.get_mut().insert(cx.add("é🙂abcd"), &cx)?;
        let key: Object = intern("roadmap-single-char-key", &cx).into();
        let other_key: Object = intern("roadmap-single-char-other", &cx).into();
        let value = cx.add("same");
        put_text_property(2, 5, key, value, None, env, &cx)?;
        put_text_property(3, 4, other_key, cx.add(9), None, env, &cx)?;
        for (position, expected) in [(1, 2), (2, 5), (5, 7), (7, 7)] {
            assert_eq!(
                next_single_char_property_change(position, key, None, None, env, &cx)?,
                expected
            );
        }
        for (position, expected) in [(7, 5), (5, 2), (2, 1), (1, 1)] {
            assert_eq!(
                previous_single_char_property_change(position, key, None, None, env, &cx)?,
                expected
            );
        }
        put_text_property(3, 4, key, cx.add("same"), None, env, &cx)?;
        assert_eq!(next_single_char_property_change(2, key, None, None, env, &cx)?, 3);
        assert_eq!(previous_single_char_property_change(5, key, None, None, env, &cx)?, 4);
        assert_eq!(next_single_char_property_change(2, key, None, Some(-1), env, &cx)?, -1);
        assert_eq!(next_single_char_property_change(5, key, None, Some(99), env, &cx)?, 7);
        assert_eq!(previous_single_char_property_change(2, key, None, Some(-1), env, &cx)?, 1);
        assert_eq!(previous_single_char_property_change(7, key, None, Some(6), env, &cx)?, 6);
        let other = get_buffer_create(cx.add("roadmap-single-char-buffer"), None, &cx)?;
        other.buffer().unwrap().lock()?.insert(cx.add("abcdef"), &cx)?;
        put_text_property(3, 6, key, cx.add(1), Some(other), env, &cx)?;
        assert_eq!(next_single_char_property_change(1, key, Some(other), None, env, &cx)?, 3);
        assert_eq!(previous_single_char_property_change(7, key, Some(other), None, env, &cx)?, 6);
        assert!(next_single_char_property_change(0, key, None, None, env, &cx).is_err());
        assert!(previous_single_char_property_change(8, key, None, None, env, &cx).is_err());
        Ok(())
    }

    #[test]
    fn roadmap_m0_single_character_property_scans_lisp_endpoints_and_limits() {
        crate::interpreter::assert_lisp(
            "(progn (insert \"abcdef\") (put-text-property 2 5 'a 1) (put-text-property 3 4 'b 2) (list (next-single-char-property-change 2 'a) (previous-single-char-property-change 5 'a) (next-single-char-property-change 5 'a nil 99) (previous-single-char-property-change 2 'a nil -1)))",
            "(5 2 7 1)",
        );
    }

    #[test]
    fn test_remove_text_properties() -> Result<()> {
        let roots = &RootSet::default();
        let mut context = Context::new(roots);
        let cx = &mut context;
        root!(env, new(Env), cx);

        let buf = get_buffer_create(cx.add("test_remove_text_properties"), None, cx)?;
        if let ObjectType::Buffer(b) = buf.untag() {
            b.lock().unwrap().get_mut().text.insert("test text");
        }

        let a = intern(":a", cx);
        let a = cx.add(a);
        let b = intern(":b", cx);
        let b = cx.add(b);

        // Add properties
        put_text_property(1, 5, a, cx.add(1), Some(buf), env, cx)?;
        put_text_property(5, 10, b, cx.add(2), Some(buf), env, cx)?;

        // Remove property :a; the supplied value is ignored.
        remove_text_properties(1, 10, list![a, 999; cx], Some(buf), env, cx)?;

        // Verify :a was removed
        let props = text_properties_at(3, Some(buf), env, &cx)?;
        assert!(props.is_nil());

        // Verify :b remains
        let props = text_properties_at(5, Some(buf), env, &cx)?;
        let val = plist_get(props, b)?;
        assert_eq!(val, cx.add(2));

        // Try removing non-existent property
        remove_text_properties(1, 10, list![a, NIL; cx], Some(buf), env, cx)?;

        BUFFERS.lock().unwrap().clear();
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_reads_reuse_current_buffer_guard() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let current = cx.add(env.current_buffer.buf_ref);
        env.current_buffer.get_mut().insert(cx.add("abc"), &cx)?;
        let key: Object = intern("roadmap-current-property", &cx).into();
        let value = cx.add("identity");
        put_text_property(1, 3, key, value, None, env, &cx)?;
        // Env already owns this buffer's guard. Explicit current-buffer reads
        // must use it rather than trying to lock the same mutex again.
        let implicit = text_properties_at(1, None, env, &cx)?;
        root!(implicit, cx);
        let explicit = text_properties_at(1, Some(current), env, &cx)?;
        assert!(eq(implicit.bind(&cx), explicit));
        assert!(eq(get_text_property(2, key, Some(current), env, &cx)?, value));
        assert!(text_properties_at(4, Some(current), env, &cx)?.is_nil());

        let deleted = get_buffer_create(cx.add("roadmap-deleted-property"), None, &cx)?;
        deleted.buffer().unwrap().lock()?.kill();
        assert!(text_properties_at(1, Some(deleted), env, &cx).is_err());
        assert!(text_properties_at(1, Some(cx.add(123)), env, &cx).is_err());
        assert!(text_properties_at(0, Some(cx.add("string")), env, &cx).is_err());
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_reads_validate_character_positions() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-property-read-bounds"), None, &cx)?;
        let key: Object = intern("roadmap-read-bounds-key", &cx).into();
        // An empty buffer has one legal position, but no character there.
        assert!(text_properties_at(1, Some(buf), env, &cx)?.is_nil());
        assert!(text_properties_at(0, Some(buf), env, &cx).is_err());
        assert!(text_properties_at(2, Some(buf), env, &cx).is_err());
        buf.buffer().unwrap().lock()?.insert(cx.add("é🙂"), &cx)?;
        put_text_property(1, 3, key, cx.add(7), Some(buf), env, &cx)?;
        assert_eq!(get_text_property(2, key, Some(buf), env, &cx)?, 7);
        assert!(text_properties_at(3, Some(buf), env, &cx)?.is_nil());
        assert!(get_text_property(3, key, Some(buf), env, &cx)?.is_nil());
        // Character counts, not UTF-8 bytes, determine the legal endpoint.
        assert!(get_text_property(4, key, Some(buf), env, &cx).is_err());
        assert!(get_text_property(0, key, Some(buf), env, &cx).is_err());
        env.set_buffer(buf.buffer().unwrap());
        assert_eq!(get_text_property(2, key, None, env, &cx)?, 7);
        assert!(text_properties_at(0, None, env, &cx).is_err());
        assert!(text_properties_at(3, None, env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn test_text_properties_at() -> Result<()> {
        let roots = &RootSet::default();
        let mut context = Context::new(roots);
        let cx = &mut context;
        root!(env, new(Env), cx);

        let buf = get_buffer_create(cx.add("test_text_properties_at"), None, cx)?;
        let n = text_properties_at(1, Some(buf), env, &cx)?;
        assert!(n.is_nil());

        let a = intern(":a", cx);
        let a = cx.add(a);
        buf.buffer().unwrap().lock()?.insert(cx.add("x"), &cx)?;
        put_text_property(1, 2, a, cx.add(3), Some(buf), env, cx)?;
        let n = text_properties_at(1, Some(buf), env, &cx)?;
        let val = plist_get(n, a)?;
        assert!(eq(val, cx.add(3)));

        BUFFERS.lock().unwrap().clear();
        Ok(())
    }
}
