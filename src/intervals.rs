use anyhow::{Result, bail};
use interval_tree::{IntervalTree as Tree, Node, StackIterator};

use crate::{
    Context,
    core::{
        cons::Cons,
        error::{Type, TypeError},
        gc::{IntoRoot, Slot, Trace},
        object::{NIL, Object, ObjectType, WithLifetime},
    },
    fns::eq,
    textprops::add_properties,
};

#[derive(Debug)]
pub struct IntervalTree<'ob> {
    pub tree: Tree<Slot<Object<'ob>>>,
}

impl Trace for IntervalTree<'_> {
    fn trace(&self, state: &mut crate::core::gc::GcState) {
        self.tree.apply(&mut |x| x.trace(state));
    }
}

impl<'new> IntoRoot<IntervalTree<'new>> for IntervalTree<'_> {
    unsafe fn into_root(self) -> IntervalTree<'new> {
        self.with_lifetime()
    }
}

impl<'new> WithLifetime<'new> for IntervalTree<'_> {
    type Out = IntervalTree<'new>;
    unsafe fn with_lifetime(self) -> IntervalTree<'new> {
        let result: IntervalTree<'new> = std::mem::transmute(self);
        result
    }
}

impl<'ob> IntervalTree<'ob> {
    pub fn new() -> Self {
        IntervalTree { tree: Tree::new() }
    }

    /// Inserts a new interval with the specified range and value into the interval tree.
    ///
    /// If the interval overlaps with existing intervals, their properties will be merged
    /// using `add_properties`. The resulting object will be stored in the tree.
    pub fn insert(
        &mut self,
        start: usize,
        end: usize,
        val: Slot<Object<'ob>>,
        cx: &'ob Context,
    ) -> Result<()> {
        // Validate every overlapping plist before changing the tree. This also
        // keeps malformed input out of the infallible tree merge callback.
        for (_, existing) in self.iter(start, end) {
            if !existing.is_nil() {
                add_properties(
                    *val,
                    existing,
                    crate::textprops::PropertySetType::Append,
                    false,
                    cx,
                )?;
            }
        }
        self.tree.insert((start, end), val, |a, b| {
            add_properties(*a, *b, crate::textprops::PropertySetType::Append, false, cx)
                .map(Slot::new)
                .unwrap_or(b)
        });
        // Ensure canonical form after insertion so adjacent equal intervals are merged
        self.clean();
        Ok(())
    }

    pub fn set_properties(&mut self, start: usize, end: usize, properties: Object<'ob>) {
        self.tree.insert((start, end), Slot::new(properties), |a, _b| a);
        self.clean();
    }

    /// Shift existing property ranges after inserting unpropertized text.
    pub(crate) fn insert_text(&mut self, position: usize, chars: usize) {
        if chars != 0 {
            self.tree.advance(position, chars);
        }
    }

    /// Remove text and translate surviving ranges to the new coordinates.
    /// This stopgap rebuild is linear; the persistent text core replaces it.
    pub(crate) fn delete_text(&mut self, start: usize, end: usize) {
        if start == end {
            return;
        }
        let mut ranges = Vec::new();
        self.tree.apply_mut(&mut |node| {
            let translate = |position: usize| {
                if position <= start {
                    position
                } else if position < end {
                    start
                } else {
                    position - (end - start)
                }
            };
            let begin = translate(node.key.start);
            let finish = translate(node.key.end);
            if begin < finish {
                ranges.push((begin, finish, *node.val));
            }
        });
        self.tree = Tree::new();
        for (begin, finish, value) in ranges {
            self.tree.insert((begin, finish), Slot::new(value), |new, _old| new);
        }
        self.clean();
    }

    pub fn find(&self, position: usize) -> Option<&Node<Slot<Object<'ob>>>> {
        self.tree.find(position)
    }

    /// Deletes properties from intervals within [start..end) that match properties in `list_of_props`.
    ///
    /// This function iterates through nodes in the specified range and removes any properties
    /// that are equal (using `eq`) to those in `list_of_props`.
    ///
    /// # Arguments
    ///
    /// * `start` - Start position of the range (inclusive)
    /// * `end` - End position of the range (exclusive)
    /// * `list_of_props` - The properties to delete (as an Object containing a list of properties)
    /// * `cx` - The context used for equality testing
    pub fn delete(
        &mut self,
        start: usize,
        end: usize,
        list_of_props: Object<'ob>,
        cx: &'ob Context,
    ) -> Result<bool> {
        let props: Vec<_> = list_of_props.as_list()?.collect::<Result<_, _>>()?;
        let mut changed = false;
        // Validate all affected lists before apply_with_split can mutate anything.
        for (_, value) in self.iter(start, end) {
            if value.is_nil() {
                continue;
            }
            let mut result = value;
            for &sym in &props {
                result = remove_sym_from_props(sym, result, cx)?;
            }
            changed |= !eq(value, result);
        }
        if !changed {
            return Ok(false);
        }
        self.tree.apply_with_split(
            |val| {
                let mut result = *val;
                for &sym in &props {
                    // The preflight above has already validated these lists.
                    result = remove_sym_from_props(sym, result, cx).unwrap_or(result);
                }
                Some(unsafe { result.into_root() })
            },
            start..end,
        );
        self.clean();
        Ok(true)
    }

    pub fn clean(&mut self) {
        self.tree.clean(|a, b| properties_eq(**a, **b), |n| n.is_nil());
    }

    pub(crate) fn iter<'a>(&'a self, start: usize, end: usize) -> IntervalIntersections<'ob, 'a> {
        IntervalIntersections::new(self, start, end)
    }

    pub(crate) fn iter_reverse<'a>(
        &'a self,
        start: usize,
        end: usize,
    ) -> ReverseIntervalIntersections<'ob, 'a> {
        ReverseIntervalIntersections::new(self, start, end)
    }
}

/// Compare effective properties without confusing plist cons identity with
/// property-value identity. Lisp property values must still compare with `eq`.
fn properties_eq(a: Object<'_>, b: Object<'_>) -> bool {
    if eq(a, b) {
        return true;
    }
    fn pairs(value: Object<'_>) -> Result<Vec<(Object<'_>, Object<'_>)>> {
        let mut iter = value.as_list()?;
        let mut result = Vec::new();
        while let Some(key) = iter.next() {
            let key = key?;
            let value =
                iter.next().ok_or_else(|| anyhow::anyhow!("Odd length property list"))??;
            if !result.iter().any(|(existing, _)| eq(*existing, key)) {
                result.push((key, value));
            }
        }
        Ok(result)
    }
    let (Ok(a), Ok(b)) = (pairs(a), pairs(b)) else { return false };
    a.len() == b.len()
        && a.iter().all(|(key, value)| {
            b.iter()
                .any(|(other_key, other_value)| eq(*key, *other_key) && eq(*value, *other_value))
        })
}

/// Return a fresh property list without `sym`, preserving the original list.
fn remove_sym_from_props<'ob>(
    sym: Object<'ob>,
    props: Object<'ob>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let mut pairs = Vec::new();
    let mut removed = false;
    let mut iter = props.as_list()?;
    while let Some(key) = iter.next() {
        let key = key?;
        let value = iter.next().ok_or_else(|| anyhow::anyhow!("Odd length property list"))??;
        if !eq(key, sym) {
            pairs.push((key, value));
        } else {
            removed = true;
        }
    }
    if !removed {
        return Ok(props);
    }
    let mut result = NIL;
    for (key, value) in pairs.into_iter().rev() {
        result = Cons::new(key, Cons::new(value, result, cx), cx).into();
    }
    Ok(result)
}

/// Get the value of property `prop` from `plist`,
/// which is the plist of an interval.
/// We check for direct properties, for categories with property `prop`,
/// and for `prop` appearing on the default-text-properties list.
pub(crate) fn textget<'ob>(plist: Object<'ob>, prop: Object<'ob>) -> Result<Object<'ob>> {
    lookup_char_property(plist, prop, true)
}

/// see `lookup_char_property in intervals.c`
fn lookup_char_property<'ob>(
    mut plist: Object<'ob>,
    prop: Object<'ob>,
    _is_textprop: bool,
) -> Result<Object<'ob>> {
    if plist.is_nil() {
        // TODO should return default properties
        return Ok(NIL);
    }
    let ObjectType::Cons(_cons) = plist.untag() else {
        bail!(TypeError::new(Type::Cons, plist))
    };

    while let ObjectType::Cons(cons) = plist.untag() {
        let p = cons.car();
        let val = cons.cadr().ok_or(anyhow::anyhow!(TypeError::new(Type::Cons, prop)))?;
        if eq(prop, p) {
            return Ok(val);
        }
        match cons.cddr() {
            Some(obj) => plist = obj,
            None => break,
        }
    }
    Ok(NIL)
}

/// An iterator that yields all intersections in a given interval range,
/// including empty gaps between nodes.
pub(crate) struct IntervalIntersections<'ob, 'tree> {
    start: usize,
    end: usize,
    current: Option<&'tree Node<Slot<Object<'ob>>>>,
    iterator: StackIterator<'tree, Slot<Object<'ob>>>,
    next_start: usize,
}

impl<'ob, 'tree> IntervalIntersections<'ob, 'tree> {
    pub(crate) fn new(tree: &'tree IntervalTree<'ob>, start: usize, end: usize) -> Self {
        let current = tree.tree.find_intersect_min(start..end);
        let mut iterator = StackIterator::new(&tree.tree, current.map(|n| n.key), false);
        iterator.next(); // remove current node
        // let next_start = current.map(|n| n.key.end).unwrap_or(start);
        let next_start = start;
        Self { start, end, current, iterator, next_start }
    }
}

impl<'ob> Iterator for IntervalIntersections<'ob, '_> {
    type Item = (std::ops::Range<usize>, Object<'ob>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_start >= self.end {
            return None;
        }

        // Handle gap before first node or between nodes
        if let Some(node) = self.current
            && self.next_start < node.key.start
        {
            let gap_end = node.key.start.min(self.end);
            let result = (self.next_start..gap_end, NIL);
            self.next_start = gap_end;
            return Some(result);
        }

        // Handle current node
        if let Some(node) = self.current {
            let intersect_start = node.key.start.max(self.start);
            let intersect_end = node.key.end.min(self.end);

            if intersect_start < intersect_end {
                let result = (intersect_start..intersect_end, *node.val);
                self.next_start = intersect_end;
                self.current = self.iterator.next();
                return Some(result);
            }
        }

        // Handle final gap after last node
        if self.next_start < self.end {
            let result = (self.next_start..self.end, NIL);
            self.next_start = self.end;
            return Some(result);
        }

        None
    }
}

/// An iterator that yields all intersections in a given interval range in reverse order,
/// including empty gaps between nodes.
pub(crate) struct ReverseIntervalIntersections<'ob, 'tree> {
    start: usize,
    end: usize,
    current: Option<&'tree Node<Slot<Object<'ob>>>>,
    iterator: StackIterator<'tree, Slot<Object<'ob>>>,
    next_end: usize,
}

impl<'ob, 'tree> ReverseIntervalIntersections<'ob, 'tree> {
    pub(crate) fn new(tree: &'tree IntervalTree<'ob>, start: usize, end: usize) -> Self {
        let current = tree.tree.find_intersect_max(start..end);
        let mut iterator = StackIterator::new(&tree.tree, current.map(|n| n.key), true);
        iterator.next(); // remove current node
        let next_end = end;
        Self { start, end, current, iterator, next_end }
    }
}

impl<'ob> Iterator for ReverseIntervalIntersections<'ob, '_> {
    type Item = (std::ops::Range<usize>, Object<'ob>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_end <= self.start {
            return None;
        }

        // Handle gap after last node or between nodes
        if let Some(node) = self.current
            && self.next_end > node.key.end
        {
            let gap_start = node.key.end.max(self.start);
            let result = (gap_start..self.next_end, NIL);
            self.next_end = gap_start;
            return Some(result);
        }

        // Handle current node
        if let Some(node) = self.current {
            let intersect_start = node.key.start.max(self.start);
            let intersect_end = node.key.end.min(self.end);

            if intersect_start < intersect_end {
                let result = (intersect_start..intersect_end, *node.val);
                self.next_end = intersect_start;
                self.current = self.iterator.next();
                return Some(result);
            }
        }

        // Handle initial gap before first node
        if self.next_end > self.start {
            let result = (self.start..self.next_end, NIL);
            self.next_end = self.start;
            return Some(result);
        }

        None
    }
}

/// set `this`'s cdr to `other`'s cddr. i.e., skip a pair in an alist
#[cfg(test)]
mod tests {
    use rune_core::macros::list;

    use super::*;
    use crate::{RootSet, intern};

    #[test]
    fn test_remove_sym_from_props() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let sym = intern("foo", &cx);
        let props = list![ sym, 1, intern("bar", &cx), 2, sym, 3, intern("baz", &cx), 4; &cx];

        // Create a property list: (test 1 test 2 test 3)
        // Remove all 'test' symbols
        let result = remove_sym_from_props(sym.into(), props, &cx).unwrap();
        let rs = result.to_string();
        assert_eq!(rs, "(bar 2 baz 4)".to_string());
    }
}

#[cfg(test)]
mod reverse_interval_iterator_tests {
    use super::*;
    use crate::{RootSet, intern};

    #[test]
    fn test_empty_tree() {
        let tree = IntervalTree::new();

        let mut iter = ReverseIntervalIntersections::new(&tree, 0, 100);
        assert_eq!(iter.next(), Some((0..100, NIL)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_single_interval() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let val = intern("test", &cx);
        let mut tree = IntervalTree::new();
        tree.insert(10, 20, Slot::new(val.into()), &cx).unwrap();

        let mut iter = ReverseIntervalIntersections::new(&tree, 0, 30);
        assert_eq!(iter.next(), Some((20..30, NIL)));
        assert_eq!(iter.next(), Some((10..20, val.into())));
        assert_eq!(iter.next(), Some((0..10, NIL)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_multiple_intervals() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let val1 = intern("test1", &cx);
        let val2 = intern("test2", &cx);
        let mut tree = IntervalTree::new();
        tree.insert(10, 20, Slot::new(val1.into()), &cx).unwrap();
        tree.insert(25, 35, Slot::new(val2.into()), &cx).unwrap();

        let mut iter = ReverseIntervalIntersections::new(&tree, 0, 40);
        assert_eq!(iter.next(), Some((35..40, NIL)));
        assert_eq!(iter.next(), Some((25..35, val2.into())));
        assert_eq!(iter.next(), Some((20..25, NIL)));
        assert_eq!(iter.next(), Some((10..20, val1.into())));
        assert_eq!(iter.next(), Some((0..10, NIL)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_overlapping_intervals() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let val1 = intern("test1", &cx);
        let val2 = intern("test2", &cx);
        let mut tree = IntervalTree::new();
        tree.insert(10, 20, Slot::new(val1.into()), &cx).unwrap();
        tree.insert(20, 30, Slot::new(val1.into()), &cx).unwrap();
        tree.insert(30, 40, Slot::new(val2.into()), &cx).unwrap();

        let mut iter = ReverseIntervalIntersections::new(&tree, 0, 50);
        assert_eq!(iter.next(), Some((40..50, NIL)));
        assert_eq!(iter.next(), Some((30..40, val2.into())));
        assert_eq!(iter.next(), Some((10..30, val1.into())));
        assert_eq!(iter.next(), Some((0..10, NIL)));
        assert_eq!(iter.next(), None);
    }
}

#[cfg(test)]
mod interval_iterator_tests {
    use super::*;
    use crate::{RootSet, intern};

    #[test]
    fn test_empty_tree() {
        let tree = IntervalTree::new();

        let mut iter = IntervalIntersections::new(&tree, 0, 100);
        assert_eq!(iter.next(), Some((0..100, NIL)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_single_interval() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let val = intern("test", &cx);
        let mut tree = IntervalTree::new();
        tree.insert(10, 20, Slot::new(val.into()), &cx).unwrap();

        let mut iter = IntervalIntersections::new(&tree, 0, 30);
        assert_eq!(iter.next(), Some((0..10, NIL)));
        assert_eq!(iter.next(), Some((10..20, val.into())));
        assert_eq!(iter.next(), Some((20..30, NIL)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_multiple_intervals() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let val1 = intern("test1", &cx);
        let val2 = intern("test2", &cx);
        let mut tree = IntervalTree::new();
        tree.insert(10, 20, Slot::new(val1.into()), &cx).unwrap();
        tree.insert(25, 35, Slot::new(val2.into()), &cx).unwrap();

        let mut iter = IntervalIntersections::new(&tree, 0, 40);
        assert_eq!(iter.next(), Some((0..10, NIL)));
        assert_eq!(iter.next(), Some((10..20, val1.into())));
        assert_eq!(iter.next(), Some((20..25, NIL)));
        assert_eq!(iter.next(), Some((25..35, val2.into())));
        assert_eq!(iter.next(), Some((35..40, NIL)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_overlapping_intervals() {
        let roots = &RootSet::default();
        let cx = Context::new(roots);
        let val1 = intern("test1", &cx);
        let val2 = intern("test2", &cx);
        let mut tree = IntervalTree::new();
        tree.insert(10, 20, Slot::new(val1.into()), &cx).unwrap();
        tree.insert(20, 30, Slot::new(val1.into()), &cx).unwrap();
        tree.insert(30, 40, Slot::new(val2.into()), &cx).unwrap();
        // merge the tree
        tree.clean();

        let mut iter = IntervalIntersections::new(&tree, 0, 50);
        assert_eq!(iter.next(), Some((0..10, NIL)));
        assert_eq!(iter.next(), Some((10..30, val1.into())));
        assert_eq!(iter.next(), Some((30..40, val2.into())));
        assert_eq!(iter.next(), Some((40..50, NIL)));
        assert_eq!(iter.next(), None);
    }
}

#[cfg(test)]
mod roadmap_m0_property_regressions {
    use super::*;
    use crate::{RootSet, intern};
    use rune_core::macros::list;

    #[test]
    fn roadmap_m0_remove_keeps_shared_plist() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let key = intern("roadmap-remove", &cx);
        let other = intern("roadmap-keep", &cx);
        let original = list![other, 1, key, 2, other, 3; &cx];
        let before = original.to_string();
        let result = remove_sym_from_props(key.into(), original, &cx).unwrap();
        assert_eq!(original.to_string(), before);
        assert_eq!(result.to_string(), "(roadmap-keep 1 roadmap-keep 3)");
        assert!(remove_sym_from_props(key.into(), NIL, &cx).unwrap().is_nil());
    }

    #[test]
    fn roadmap_m0_malformed_plists_fail_before_tree_changes() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let key = intern("roadmap-malformed", &cx);
        let odd = list![key; &cx];
        let mut tree = IntervalTree::new();
        tree.set_properties(1, 5, odd);
        let valid = list![key, 1; &cx];
        assert!(tree.insert(2, 4, Slot::new(valid), &cx).is_err());
        assert_eq!(tree.find(2).map(|node| *node.val), Some(odd));
        let keys = list![key; &cx];
        assert!(tree.delete(2, 4, keys, &cx).is_err());
        assert_eq!(tree.find(2).map(|node| *node.val), Some(odd));
    }

    #[test]
    fn roadmap_m0_property_ranges_follow_text_edits() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let value: Object = intern("roadmap-property", &cx).into();
        let mut tree = IntervalTree::new();
        tree.set_properties(2, 6, value);
        tree.insert_text(4, 2);
        assert_eq!(tree.find(3).map(|n| *n.val), Some(value));
        assert!(tree.find(4).is_none());
        assert_eq!(tree.find(6).map(|n| *n.val), Some(value));
        tree.delete_text(3, 7);
        assert_eq!(tree.find(2).map(|n| *n.val), Some(value));
        assert_eq!(tree.find(3).map(|n| *n.val), Some(value));
        assert!(tree.find(4).is_none());
        tree.delete_text(1, 5);
        assert!(tree.find(1).is_none());
    }
}
