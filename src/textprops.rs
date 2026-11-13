use crate::{
    core::{
        cons::Cons,
        env::Env,
        error::{Type, TypeError},
        gc::{Context, Rt, Slot},
        object::{NIL, Object, ObjectType, WithLifetime},
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
    mut obj_i: Object<'ob>,
    _set_type: PropertySetType,
    _destructive: bool,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
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
    let mut incoming = plist.as_list()?;
    while let Some(key) = incoming.next() {
        let key = key?;
        let value = incoming.next().ok_or_else(|| anyhow!("Odd length property list"))??;
        if let Some(pair) = pairs.iter_mut().find(|pair| eq(pair.0, key)) {
            pair.1 = value;
        } else {
            pairs.push((key, value));
        }
    }
    if pairs.len() == original_pairs.len()
        && pairs.iter().zip(&original_pairs).all(|(a, b)| eq(a.0, b.0) && eq(a.1, b.1))
    {
        return Ok(obj_i);
    }
    obj_i = NIL;
    for (key, value) in pairs.into_iter().rev() {
        obj_i = Cons::new(key, Cons::new(value, obj_i, cx), cx).into();
    }
    Ok(obj_i)
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
    object: Object<'ob>,
    env: &'ob mut Rt<Env>,
) -> Result<Object<'ob>> {
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
        let properties = data.textprops.find(position).map(|node| *node.val).unwrap_or(NIL);
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
    object: Object<'ob>,
    env: &'ob mut Rt<Env>,
) -> Result<Object<'ob>> {
    let props = text_properties_at(position, object, env)?;
    // TODO see lookup_char_property, should also lookup
    // 1. category
    // 2. char_property_alias_alist
    // 3.  default_text_properties
    textget(props, prop)
}

#[defun]
#[allow(unused)]
pub fn get_char_property_and_overlay<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Object<'ob>,
    env: &'ob mut Rt<Env>,
) -> Result<Object<'ob>> {
    todo!()
}

#[defun]
#[allow(unused)]
pub fn get_char_property<'ob>(
    position: usize,
    prop: Object<'ob>,
    object: Object<'ob>,
    env: &'ob mut Rt<Env>,
) -> Result<Object<'ob>> {
    todo!()
}

// TODO also missing `next-char-property-change` and 3 other similar functions.

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
    let prop = Slot::new(prop);
    modify_buffer_data(object, env, |data| {
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(());
        }
        let tree = &mut data.textprops_with_lifetime();
        tree.insert(start, end, prop, cx)
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
    object: Object<'ob>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_max = data.text.len_chars() + 1;
        let end = limit.unwrap_or(point_max);
        let tree = data.textprops_with_lifetime();
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
    object: Object<'ob>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_max = data.text.len_chars() + 1;
        let end = limit.unwrap_or(point_max);
        let tree = data.textprops_with_lifetime();
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
    object: Object<'ob>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_min = 1;
        let start = limit.unwrap_or(point_min);
        let end = position;
        let tree = data.textprops_with_lifetime();
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
    object: Object<'ob>,
    limit: Option<usize>,
    env: &'ob mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let point_min = 1;
        let start = limit.unwrap_or(point_min);
        let tree = data.textprops_with_lifetime();
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
) -> Result<()> {
    let object = object.unwrap_or(NIL);
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<()> {
        if !validate_property_mutation_range(data, start, end)? {
            return Ok(());
        }
        let tree = data.textprops_with_lifetime();
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
        let tree = data.textprops_with_lifetime();
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
        let tree = data.textprops_with_lifetime();
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
    object: Object<'ob>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let tree = data.textprops_with_lifetime();
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
    object: Object<'ob>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let (start, end) = (start.min(end), start.max(end));
    modify_buffer_data(object, env, |data| -> Result<Object<'ob>> {
        let tree = data.textprops_with_lifetime();
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
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdefg"))?;
        let key: Object = intern("roadmap-face", &cx).into();
        let missing: Object = intern("roadmap-missing", &cx).into();
        let value = cx.add("bold");
        put_text_property(1, 7, key, value, Some(buf), env, &cx)?;
        put_text_property(2, 4, key, value, Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, buf, None, env, &cx)?, 7);
        remove_list_of_text_properties(2, 4, list![missing; &cx], Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, buf, None, env, &cx)?, 7);
        // Equal-looking strings are not eq and must retain a boundary.
        put_text_property(2, 4, key, cx.add("bold"), Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, buf, None, env, &cx)?, 2);
        put_text_property(2, 4, key, value, Some(buf), env, &cx)?;
        assert_eq!(next_property_change(1, buf, None, env, &cx)?, 7);
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
        let change = next_property_change(1, buf, None, env, cx)?;
        assert_eq!(change, cx.add(5)); // Change at end of first range

        let change = next_property_change(5, buf, None, env, cx)?;
        assert_eq!(change, cx.add(10)); // Change at end of second range

        let change = next_property_change(10, buf, None, env, cx)?;
        assert_eq!(change, cx.add(15)); // Change at start of third range

        let change = next_property_change(15, buf, None, env, cx)?;
        assert_eq!(change, cx.add(20)); // Change at end of third range

        // Test with limit
        let change = next_property_change(1, buf, Some(8), env, cx)?;
        assert_eq!(change, cx.add(5)); // Change within limit

        let change = next_property_change(5, buf, Some(8), env, cx)?;
        assert_eq!(change, cx.add(8)); // Limit reached

        // Test no change case
        let change = next_property_change(20, buf, None, env, cx)?;
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
        let change = next_single_property_change(1, a, buf, None, env, cx)?;
        assert_eq!(change, cx.add(5)); // Change at end of first range

        let change = next_single_property_change(5, a, buf, None, env, cx)?;
        assert_eq!(change, cx.add(8)); // Change at end of second range

        let change = next_single_property_change(10, a, buf, None, env, cx)?;
        assert_eq!(change, cx.add(15)); // Change at start of third range

        let change = next_single_property_change(15, a, buf, None, env, cx)?;
        assert_eq!(change, cx.add(20)); // Change at end of third range

        // Test with limit
        let change = next_single_property_change(1, a, buf, Some(8), env, cx)?;
        assert_eq!(change, cx.add(5)); // Change within limit

        let change = next_single_property_change(5, a, buf, Some(8), env, cx)?;
        assert_eq!(change, cx.add(8)); // Limit reached

        // Test no change case
        let change = next_single_property_change(20, a, buf, None, env, cx)?;
        assert!(change.is_nil()); // No changes after last property

        // Test property changes for :b
        let change = next_single_property_change(1, b, buf, None, env, cx)?;
        assert_eq!(change, cx.add(10)); // Change at start of b range

        let change = next_single_property_change(10, b, buf, None, env, cx)?;
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
        buf.buffer().unwrap().lock()?.insert(cx.add("abcde"))?;
        let a: Object = intern("roadmap-remove-a", &cx).into();
        let b: Object = intern("roadmap-remove-b", &cx).into();
        put_text_property(1, 5, a, cx.add(1), Some(buf), env, &cx)?;
        put_text_property(1, 5, b, cx.add(2), Some(buf), env, &cx)?;
        // GNU's CONSP candidate loop ignores a bare symbol, rather than
        // interpreting it as a property name or rejecting it.
        remove_text_properties(2, 4, a, Some(buf), env, &cx)?;
        assert_eq!(plist_get(text_properties_at(2, buf, env)?, a)?, 1);
        assert_eq!(next_property_change(1, buf, None, env, &cx)?, 5);
        // A final proper-list key is still a removal candidate with no value.
        remove_text_properties(2, 4, list![a; &cx], Some(buf), env, &cx)?;
        assert!(plist_get(text_properties_at(2, buf, env)?, a)?.is_nil());
        assert_eq!(plist_get(text_properties_at(1, buf, env)?, a)?, 1);
        assert_eq!(plist_get(text_properties_at(4, buf, env)?, a)?, 1);
        assert_eq!(plist_get(text_properties_at(2, buf, env)?, b)?, 2);
        Ok(())
    }

    #[test]
    fn roadmap_m0_previous_single_change_reports_shared_boundary() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-previous-single"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdefghij"))?;
        let key: Object = intern("roadmap-previous-key", &cx).into();
        let other: Object = intern("roadmap-previous-other", &cx).into();
        let value = cx.add("value");
        put_text_property(2, 5, key, value, Some(buf), env, &cx)?;
        put_text_property(5, 8, key, cx.add("value"), Some(buf), env, &cx)?;
        // Equal-looking but non-eq strings have a real boundary at 5.
        assert_eq!(previous_single_property_change(7, key, buf, None, env, &cx)?, 5);
        assert_eq!(previous_single_property_change(5, key, buf, None, env, &cx)?, 2);
        // A gap after a property range changes at the range's end, not start.
        assert_eq!(previous_single_property_change(10, key, buf, None, env, &cx)?, 8);
        // Unrelated boundaries do not interrupt the specific property scan.
        put_text_property(3, 4, other, cx.add(1), Some(buf), env, &cx)?;
        assert_eq!(previous_single_property_change(5, key, buf, None, env, &cx)?, 2);
        assert_eq!(previous_single_property_change(7, key, buf, Some(6), env, &cx)?, 6);
        assert!(previous_single_property_change(2, key, buf, None, env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_reversed_property_mutations_normalize_ranges() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-reversed-properties"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"))?;
        let a: Object = intern("roadmap-reversed-a", &cx).into();
        let b: Object = intern("roadmap-reversed-b", &cx).into();
        put_text_property(6, 2, a, cx.add(1), Some(buf), env, &cx)?;
        assert!(get_text_property(1, a, buf, env)?.is_nil());
        assert_eq!(get_text_property(2, a, buf, env)?, 1);
        assert_eq!(get_text_property(5, a, buf, env)?, 1);
        assert!(get_text_property(6, a, buf, env)?.is_nil());
        set_text_properties(5, 3, list![b, 2; &cx], Some(buf), env)?;
        assert!(get_text_property(3, a, buf, env)?.is_nil());
        assert_eq!(get_text_property(3, b, buf, env)?, 2);
        assert_eq!(get_text_property(2, a, buf, env)?, 1);
        assert_eq!(get_text_property(5, a, buf, env)?, 1);
        remove_text_properties(4, 3, list![b, NIL; &cx], Some(buf), env, &cx)?;
        assert!(text_properties_at(3, buf, env)?.is_nil());
        assert_eq!(get_text_property(4, b, buf, env)?, 2);
        remove_list_of_text_properties(6, 4, list![a, b; &cx], Some(buf), env, &cx)?;
        assert!(text_properties_at(4, buf, env)?.is_nil());
        assert!(text_properties_at(5, buf, env)?.is_nil());
        assert_eq!(get_text_property(2, a, buf, env)?, 1);
        // Normalization must leave empty ranges as no-ops.
        put_text_property(2, 2, a, cx.add(9), Some(buf), env, &cx)?;
        set_text_properties(2, 2, NIL, Some(buf), env)?;
        remove_text_properties(2, 2, list![a, NIL; &cx], Some(buf), env, &cx)?;
        remove_list_of_text_properties(2, 2, list![a; &cx], Some(buf), env, &cx)?;
        assert_eq!(get_text_property(2, a, buf, env)?, 1);
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_search_predicates_and_identity() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-property-search"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"))?;
        let key: Object = intern("roadmap-search-key", &cx).into();
        let value = cx.add("same");
        put_text_property(2, 5, key, value, Some(buf), env, &cx)?;
        assert_eq!(text_property_any(1, 7, key, value, buf, env, &cx)?, 2);
        assert_eq!(text_property_any(3, 7, key, value, buf, env, &cx)?, 3);
        assert!(text_property_any(1, 7, key, cx.add("same"), buf, env, &cx)?.is_nil());
        assert_eq!(text_property_not_all(1, 7, key, value, buf, env, &cx)?, 1);
        assert_eq!(text_property_not_all(2, 7, key, value, buf, env, &cx)?, 5);
        assert!(text_property_not_all(2, 5, key, value, buf, env, &cx)?.is_nil());
        assert_eq!(text_property_any(2, 7, key, NIL, buf, env, &cx)?, 5);
        assert_eq!(text_property_not_all(1, 7, key, NIL, buf, env, &cx)?, 2);
        assert_eq!(text_property_any(7, 1, key, value, buf, env, &cx)?, 2);
        assert_eq!(text_property_not_all(7, 2, key, value, buf, env, &cx)?, 5);
        assert!(text_property_any(3, 3, key, value, buf, env, &cx)?.is_nil());
        assert!(text_property_not_all(3, 3, key, NIL, buf, env, &cx)?.is_nil());
        Ok(())
    }

    #[test]
    fn roadmap_m0_property_removal_reports_actual_changes() -> Result<()> {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        root!(env, new(Env), cx);
        let buf = get_buffer_create(cx.add("roadmap-removal-results"), None, &cx)?;
        buf.buffer().unwrap().lock()?.insert(cx.add("abcdef"))?;
        let key: Object = intern("roadmap-removal-result-key", &cx).into();
        let missing: Object = intern("roadmap-removal-result-missing", &cx).into();
        put_text_property(1, 6, key, NIL, Some(buf), env, &cx)?;
        assert!(!remove_text_properties(2, 4, key, Some(buf), env, &cx)?);
        assert!(!remove_text_properties(2, 4, list![missing, 1; &cx], Some(buf), env, &cx)?);
        assert!(!remove_list_of_text_properties(2, 2, list![key; &cx], Some(buf), env, &cx)?);
        // Presence, not a non-nil value, determines whether removal changed text.
        assert!(remove_text_properties(2, 4, list![key, 999; &cx], Some(buf), env, &cx)?);
        assert!(!remove_text_properties(2, 4, list![key, NIL; &cx], Some(buf), env, &cx)?);
        assert!(!text_properties_at(1, buf, env)?.is_nil());
        assert!(text_properties_at(2, buf, env)?.is_nil());
        assert!(!text_properties_at(4, buf, env)?.is_nil());
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
        env.current_buffer.get_mut().insert(cx.add("abc"))?;
        let other = get_buffer_create(cx.add("roadmap-optional-other"), None, &cx)?;
        other.buffer().unwrap().lock()?.insert(cx.add("abc"))?;
        let key: Object = intern("roadmap-optional-key", &cx).into();
        put_text_property(1, 3, key, cx.add(1), None, env, &cx)?;
        set_text_properties(1, 3, list![key, 2; &cx], Some(other), env)?;
        assert_eq!(get_text_property(1, key, NIL, env)?, 1);
        assert_eq!(get_text_property(1, key, other, env)?, 2);
        assert!(remove_text_properties(1, 3, list![key, NIL; &cx], None, env, &cx)?);
        assert_eq!(get_text_property(1, key, other, env)?, 2);
        assert!(remove_list_of_text_properties(1, 3, list![key; &cx], Some(other), env, &cx)?);
        assert!(get_text_property(1, key, other, env)?.is_nil());
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
        buf.buffer().unwrap().lock()?.insert(cx.add("é🙂"))?;
        let key: Object = intern("roadmap-mutation-bounds-key", &cx).into();
        put_text_property(1, 3, key, cx.add(7), Some(buf), env, &cx)?;
        for (start, end) in [(0, 2), (1, 4), (4, 1), (1, usize::MAX)] {
            assert!(put_text_property(start, end, key, cx.add(8), Some(buf), env, &cx).is_err());
            assert!(set_text_properties(start, end, NIL, Some(buf), env).is_err());
            assert!(
                remove_text_properties(start, end, list![key, NIL; &cx], Some(buf), env, &cx)
                    .is_err()
            );
            assert!(
                remove_list_of_text_properties(start, end, list![key; &cx], Some(buf), env, &cx)
                    .is_err()
            );
            assert_eq!(get_text_property(1, key, buf, env)?, 7);
            assert_eq!(get_text_property(2, key, buf, env)?, 7);
            assert!(text_properties_at(3, buf, env)?.is_nil());
        }
        // GNU returns without validating the bounds of an empty range.
        put_text_property(usize::MAX, usize::MAX, key, NIL, Some(buf), env, &cx)?;
        set_text_properties(0, 0, NIL, Some(buf), env)?;
        assert!(!remove_text_properties(0, 0, list![key; &cx], Some(buf), env, &cx)?);
        assert!(!remove_list_of_text_properties(
            usize::MAX,
            usize::MAX,
            list![key; &cx],
            Some(buf),
            env,
            &cx
        )?);
        assert_eq!(get_text_property(1, key, buf, env)?, 7);
        // Endpoint3 is legal for two Unicode characters, regardless of bytes.
        assert!(remove_list_of_text_properties(3, 1, list![key; &cx], Some(buf), env, &cx)?);
        assert!(text_properties_at(1, buf, env)?.is_nil());
        Ok(())
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
        let props = text_properties_at(3, buf, env)?;
        assert!(props.is_nil());

        // Verify :b remains
        let props = text_properties_at(5, buf, env)?;
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
        env.current_buffer.get_mut().insert(cx.add("abc"))?;
        let key: Object = intern("roadmap-current-property", &cx).into();
        let value = cx.add("identity");
        put_text_property(1, 3, key, value, None, env, &cx)?;
        // Env already owns this buffer's guard. Explicit current-buffer reads
        // must use it rather than trying to lock the same mutex again.
        let implicit = text_properties_at(1, NIL, env)?;
        root!(implicit, cx);
        let explicit = text_properties_at(1, current, env)?;
        assert!(eq(implicit.bind(&cx), explicit));
        assert!(eq(get_text_property(2, key, current, env)?, value));
        assert!(text_properties_at(4, current, env)?.is_nil());

        let deleted = get_buffer_create(cx.add("roadmap-deleted-property"), None, &cx)?;
        deleted.buffer().unwrap().lock()?.kill();
        assert!(text_properties_at(1, deleted, env).is_err());
        assert!(text_properties_at(1, cx.add(123), env).is_err());
        assert!(text_properties_at(0, cx.add("string"), env).is_err());
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
        assert!(text_properties_at(1, buf, env)?.is_nil());
        assert!(text_properties_at(0, buf, env).is_err());
        assert!(text_properties_at(2, buf, env).is_err());
        buf.buffer().unwrap().lock()?.insert(cx.add("é🙂"))?;
        put_text_property(1, 3, key, cx.add(7), Some(buf), env, &cx)?;
        assert_eq!(get_text_property(2, key, buf, env)?, 7);
        assert!(text_properties_at(3, buf, env)?.is_nil());
        assert!(get_text_property(3, key, buf, env)?.is_nil());
        // Character counts, not UTF-8 bytes, determine the legal endpoint.
        assert!(get_text_property(4, key, buf, env).is_err());
        assert!(get_text_property(0, key, buf, env).is_err());
        env.set_buffer(buf.buffer().unwrap());
        assert_eq!(get_text_property(2, key, NIL, env)?, 7);
        assert!(text_properties_at(0, NIL, env).is_err());
        assert!(text_properties_at(3, NIL, env)?.is_nil());
        Ok(())
    }

    #[test]
    fn test_text_properties_at() -> Result<()> {
        let roots = &RootSet::default();
        let mut context = Context::new(roots);
        let cx = &mut context;
        root!(env, new(Env), cx);

        let buf = get_buffer_create(cx.add("test_text_properties_at"), None, cx)?;
        let n = text_properties_at(1, buf, env)?;
        assert!(n.is_nil());

        let a = intern(":a", cx);
        let a = cx.add(a);
        buf.buffer().unwrap().lock()?.insert(cx.add("x"))?;
        put_text_property(1, 2, a, cx.add(3), Some(buf), env, cx)?;
        let n = text_properties_at(1, buf, env)?;
        let val = plist_get(n, a)?;
        assert!(eq(val, cx.add(3)));

        BUFFERS.lock().unwrap().clear();
        Ok(())
    }
}
