//! Persistent, summarized ordered sequences for text, runs and layout.
//! Clones share nodes; split/concat copy only balanced tree paths. No GC pointers.
#![forbid(unsafe_code)]
use std::{ops::Range, sync::Arc};

/// An associative summary with an identity. Order matters for `combine`.
pub trait Summary: Clone + Default {
    fn combine(&self, right: &Self) -> Self;
}
pub trait Summarize {
    type Summary: Summary;
    fn summarize(&self) -> Self::Summary;
}

#[derive(Debug)]
enum Node<T: Summarize> {
    Leaf {
        value: T,
        summary: T::Summary,
    },
    Branch {
        left: Arc<Node<T>>,
        right: Arc<Node<T>>,
        summary: T::Summary,
        len: usize,
        height: u32,
    },
}
impl<T: Summarize> Node<T> {
    fn len(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Branch { len, .. } => *len,
        }
    }
    fn height(&self) -> u32 {
        match self {
            Self::Leaf { .. } => 1,
            Self::Branch { height, .. } => *height,
        }
    }
    fn summary(&self) -> &T::Summary {
        match self {
            Self::Leaf { summary, .. } | Self::Branch { summary, .. } => summary,
        }
    }
}
type Link<T> = Option<Arc<Node<T>>>;
fn branch<T: Summarize>(left: Arc<Node<T>>, right: Arc<Node<T>>) -> Arc<Node<T>> {
    Arc::new(Node::Branch {
        summary: left.summary().combine(right.summary()),
        len: left.len().checked_add(right.len()).expect("tree item capacity overflow"),
        height: 1 + left.height().max(right.height()),
        left,
        right,
    })
}
fn balance<T: Summarize>(left: Arc<Node<T>>, right: Arc<Node<T>>) -> Arc<Node<T>> {
    if left.height() > right.height() + 1 {
        let Node::Branch { left: a, right: b, .. } = left.as_ref() else { unreachable!() };
        if a.height() >= b.height() {
            branch(a.clone(), branch(b.clone(), right))
        } else {
            let Node::Branch { left: c, right: d, .. } = b.as_ref() else { unreachable!() };
            branch(branch(a.clone(), c.clone()), branch(d.clone(), right))
        }
    } else if right.height() > left.height() + 1 {
        let Node::Branch { left: a, right: b, .. } = right.as_ref() else { unreachable!() };
        if b.height() >= a.height() {
            branch(branch(left, a.clone()), b.clone())
        } else {
            let Node::Branch { left: c, right: d, .. } = a.as_ref() else { unreachable!() };
            branch(branch(left, c.clone()), branch(d.clone(), b.clone()))
        }
    } else {
        branch(left, right)
    }
}
fn join<T: Summarize>(left: Link<T>, right: Link<T>) -> Link<T> {
    match (left, right) {
        (None, other) | (other, None) => other,
        (Some(left), Some(right)) => {
            if left.height() > right.height() + 1 {
                let Node::Branch { left: a, right: b, .. } = left.as_ref() else { unreachable!() };
                Some(balance(a.clone(), join(Some(b.clone()), Some(right)).unwrap()))
            } else if right.height() > left.height() + 1 {
                let Node::Branch { left: a, right: b, .. } = right.as_ref() else { unreachable!() };
                Some(balance(join(Some(left), Some(a.clone())).unwrap(), b.clone()))
            } else {
                Some(branch(left, right))
            }
        }
    }
}
fn split<T: Summarize>(node: Link<T>, at: usize) -> (Link<T>, Link<T>) {
    let Some(node) = node else { return (None, None) };
    if at == 0 {
        return (None, Some(node));
    }
    if at == node.len() {
        return (Some(node), None);
    }
    let Node::Branch { left, right, .. } = node.as_ref() else { unreachable!() };
    if at < left.len() {
        let (a, b) = split(Some(left.clone()), at);
        (a, join(b, Some(right.clone())))
    } else {
        let (a, b) = split(Some(right.clone()), at - left.len());
        (join(Some(left.clone()), a), b)
    }
}

/// Appending would exceed the addressable item-count coordinate space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeCapacityError;
impl std::fmt::Display for TreeCapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tree item capacity overflow")
    }
}
impl std::error::Error for TreeCapacityError {}

/// A persistent AVL sequence. Items may themselves share immutable storage.
pub struct SumTree<T: Summarize> {
    root: Link<T>,
}
impl<T: Summarize> Clone for SumTree<T> {
    fn clone(&self) -> Self {
        Self { root: self.root.clone() }
    }
}
impl<T: Summarize> Default for SumTree<T> {
    fn default() -> Self {
        Self { root: None }
    }
}
impl<T: Summarize> SumTree<T> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn singleton(value: T) -> Self {
        let summary = value.summarize();
        Self { root: Some(Arc::new(Node::Leaf { value, summary })) }
    }
    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.len())
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    pub fn summary(&self) -> T::Summary {
        self.root.as_ref().map_or_else(T::Summary::default, |n| n.summary().clone())
    }
    /// Panics if the combined item count exceeds usize::MAX. Payload-summary
    /// arithmetic remains the responsibility of the Summary implementation.
    pub fn append(&self, right: &Self) -> Self {
        self.checked_append(right).expect("tree item capacity overflow")
    }
    /// Validate item capacity before joining either tree. Neither input is
    /// modified on failure, including for arbitrarily shared zero-weight items.
    pub fn checked_append(&self, right: &Self) -> Result<Self, TreeCapacityError> {
        self.len().checked_add(right.len()).ok_or(TreeCapacityError)?;
        Ok(Self { root: join(self.root.clone(), right.root.clone()) })
    }
    /// Split between items. An invalid index does not change either input.
    pub fn split(&self, at: usize) -> Option<(Self, Self)> {
        if at > self.len() {
            return None;
        }
        let (left, right) = split(self.root.clone(), at);
        Some((Self { root: left }, Self { root: right }))
    }
    pub fn get(&self, mut index: usize) -> Option<&T> {
        let mut node = self.root.as_deref()?;
        loop {
            match node {
                Node::Leaf { value, .. } => return (index == 0).then_some(value),
                Node::Branch { left, right, .. } => {
                    if index < left.len() {
                        node = left;
                    } else {
                        index -= left.len();
                        node = right;
                    }
                }
            }
        }
    }
    /// Seek by a nonnegative additive summary dimension. Returns the item,
    /// its index and the combined summary strictly before it. Endpoints return None.
    pub fn seek(
        &self,
        mut offset: usize,
        dimension: impl Fn(&T::Summary) -> usize,
    ) -> Option<(usize, T::Summary, &T)> {
        let mut node = self.root.as_deref()?;
        if offset >= dimension(node.summary()) {
            return None;
        }
        let mut index = 0;
        let mut before = T::Summary::default();
        loop {
            match node {
                Node::Leaf { value, .. } => return Some((index, before, value)),
                Node::Branch { left, right, .. } => {
                    let size = dimension(left.summary());
                    if offset < size {
                        node = left;
                    } else {
                        offset -= size;
                        index += left.len();
                        before = before.combine(left.summary());
                        node = right;
                    }
                }
            }
        }
    }
    /// First item whose inclusive cumulative end reaches `offset`. Unlike
    /// `seek`, this includes zero-width items and equal-position boundaries.
    pub fn seek_end(
        &self,
        mut offset: usize,
        dimension: impl Fn(&T::Summary) -> usize,
    ) -> Option<(usize, T::Summary, &T)> {
        let mut node = self.root.as_deref()?;
        if offset > dimension(node.summary()) {
            return None;
        }
        let mut index = 0;
        let mut before = T::Summary::default();
        loop {
            match node {
                Node::Leaf { value, .. } => return Some((index, before, value)),
                Node::Branch { left, right, .. } => {
                    let size = dimension(left.summary());
                    if offset <= size {
                        node = left;
                    } else {
                        offset -= size;
                        index += left.len();
                        before = before.combine(left.summary());
                        node = right;
                    }
                }
            }
        }
    }
    pub fn iter(&self) -> Iter<'_, T> {
        self.iter_range(0..self.len()).unwrap()
    }
    pub fn iter_range(&self, range: Range<usize>) -> Option<Iter<'_, T>> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        let stack = self.root.as_deref().map(|n| vec![(n, 0)]).unwrap_or_default();
        Some(Iter { front: stack.clone(), back: stack, remaining: range.end - range.start, range })
    }
}
impl<T: Summarize> FromIterator<T> for SumTree<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut nodes: Vec<_> = iter
            .into_iter()
            .map(|value| {
                let summary = value.summarize();
                Arc::new(Node::Leaf { value, summary })
            })
            .collect();
        while nodes.len() > 1 {
            let mut iter = nodes.into_iter();
            let mut next = Vec::new();
            while let Some(left) = iter.next() {
                next.push(match iter.next() {
                    Some(right) => join(Some(left), Some(right)).unwrap(),
                    None => left,
                });
            }
            nodes = next;
        }
        Self { root: nodes.pop() }
    }
}
pub struct Iter<'a, T: Summarize> {
    front: Vec<(&'a Node<T>, usize)>,
    back: Vec<(&'a Node<T>, usize)>,
    range: Range<usize>,
    remaining: usize,
}
fn next_leaf<'a, T: Summarize>(
    stack: &mut Vec<(&'a Node<T>, usize)>,
    range: &Range<usize>,
    reverse: bool,
) -> Option<&'a T> {
    while let Some((node, base)) = stack.pop() {
        if base >= range.end || base + node.len() <= range.start {
            continue;
        }
        match node {
            Node::Leaf { value, .. } => return Some(value),
            Node::Branch { left, right, .. } => {
                let l = (left.as_ref(), base);
                let r = (right.as_ref(), base + left.len());
                if reverse {
                    stack.push(l);
                    stack.push(r);
                } else {
                    stack.push(r);
                    stack.push(l);
                }
            }
        }
    }
    None
}
impl<'a, T: Summarize> Iterator for Iter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        next_leaf(&mut self.front, &self.range, false)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<T: Summarize> DoubleEndedIterator for Iter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        next_leaf(&mut self.back, &self.range, true)
    }
}
impl<T: Summarize> ExactSizeIterator for Iter<'_, T> {}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Default, Debug, PartialEq)]
    pub struct Count(usize);
    impl Summary for Count {
        fn combine(&self, other: &Self) -> Self {
            Self(self.0 + other.0)
        }
    }
    impl Summarize for usize {
        type Summary = Count;
        fn summarize(&self) -> Count {
            Count(*self)
        }
    }
    fn check(node: &Node<usize>) -> (usize, u32, usize) {
        match node {
            Node::Leaf { value, summary } => {
                assert_eq!(summary.0, *value);
                (1, 1, *value)
            }
            Node::Branch { left, right, summary, len, height } => {
                let (a, ha, sa) = check(left);
                let (b, hb, sb) = check(right);
                assert!(ha.abs_diff(hb) <= 1);
                assert_eq!(*len, a + b);
                assert_eq!(*height, 1 + ha.max(hb));
                assert_eq!(summary.0, sa + sb);
                (*len, *height, summary.0)
            }
        }
    }
    #[test]
    fn persistent_split_join_and_mixed_iterator() {
        let tree: SumTree<_> = (1..1000).collect();
        let original = tree.clone();
        for at in 0..=tree.len() {
            let (a, b) = tree.split(at).unwrap();
            let joined = a.append(&b);
            check(joined.root.as_deref().unwrap());
            assert_eq!(joined.iter().copied().collect::<Vec<_>>(), (1..1000).collect::<Vec<_>>());
        }
        assert_eq!(original.summary(), Count(999 * 1000 / 2));
        let mut iter = tree.iter_range(3..8).unwrap();
        assert_eq!(iter.next(), Some(&4));
        assert_eq!(iter.next_back(), Some(&8));
        assert_eq!(iter.copied().collect::<Vec<_>>(), vec![5, 6, 7]);
    }
    #[test]
    fn repeated_append_stays_balanced_and_seeks_summaries() {
        let mut tree = SumTree::new();
        for i in 0..2000 {
            tree = tree.append(&SumTree::singleton(i % 7));
            check(tree.root.as_deref().unwrap());
        }
        let mut before = 0;
        for (i, value) in tree.iter().enumerate() {
            for offset in before..before + value {
                let (index, summary, found) = tree.seek(offset, |s| s.0).unwrap();
                assert_eq!((index, summary.0, *found), (i, before, *value));
            }
            before += value;
        }
        assert!(tree.seek(before, |s| s.0).is_none());
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[derive(Clone, Default)]
    struct Zero;
    impl Summary for Zero {
        fn combine(&self, _: &Self) -> Self {
            Self
        }
    }
    struct Item;
    impl Summarize for Item {
        type Summary = Zero;
        fn summarize(&self) -> Zero {
            Zero
        }
    }
    #[test]
    fn shared_zero_weight_items_cannot_wrap_item_count() {
        let mut power = SumTree::singleton(Item);
        let mut maximum = SumTree::new();
        for bit in 0..usize::BITS {
            maximum = maximum.checked_append(&power).unwrap();
            if bit + 1 < usize::BITS {
                power = power.checked_append(&power).unwrap();
            }
        }
        assert_eq!(maximum.len(), usize::MAX);
        assert!(maximum.get(usize::MAX - 1).is_some());
        assert!(maximum.get(usize::MAX).is_none());
        assert!(matches!(
            maximum.checked_append(&SumTree::singleton(Item)),
            Err(TreeCapacityError)
        ));
        assert!(matches!(power.checked_append(&power), Err(TreeCapacityError)));
        assert!(std::panic::catch_unwind(|| maximum.append(&power)).is_err());
        assert_eq!(maximum.checked_append(&SumTree::new()).unwrap().len(), usize::MAX);
        assert_eq!(maximum.iter_range(usize::MAX - 1..usize::MAX).unwrap().count(), 1);
    }
}
