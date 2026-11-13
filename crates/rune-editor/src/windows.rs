use crate::{BufferId, EditorError, Result, WindowId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub buffer: BufferId,
    pub point: usize,
    pub start: usize,
    pub hscroll: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowContent {
    Leaf(View),
    Split {
        axis: Axis,
        first: WindowId,
        second: WindowId,
        first_units: u32,
        total_units: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowNode {
    pub id: WindowId,
    pub parent: Option<WindowId>,
    pub rect: CellRect,
    pub content: WindowContent,
}

/// Binary combination tree. Every structural/geometry change is prepared on a
/// clone, checked for complete coverage and positive leaf sizes, then committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowTree {
    nodes: BTreeMap<WindowId, WindowNode>,
    root: WindowId,
    selected: WindowId,
    width: u32,
    height: u32,
}
impl WindowTree {
    pub fn new(buffer: BufferId, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(EditorError::InvalidGeometry);
        }
        let root = WindowId(crate::allocate_id()?);
        let node = WindowNode {
            id: root,
            parent: None,
            rect: CellRect { x: 0, y: 0, width, height },
            content: WindowContent::Leaf(View { buffer, point: 0, start: 0, hscroll: 0 }),
        };
        Ok(Self { nodes: [(root, node)].into(), root, selected: root, width, height })
    }
    pub fn root(&self) -> WindowId {
        self.root
    }
    pub fn selected(&self) -> WindowId {
        self.selected
    }
    pub fn node(&self, id: WindowId) -> Option<&WindowNode> {
        self.nodes.get(&id)
    }
    pub fn view(&self, id: WindowId) -> Result<&View> {
        match self.nodes.get(&id).map(|node| &node.content) {
            Some(WindowContent::Leaf(view)) => Ok(view),
            _ => Err(EditorError::InvalidWindow),
        }
    }
    pub(crate) fn view_mut(&mut self, id: WindowId) -> Result<&mut View> {
        match self.nodes.get_mut(&id).map(|node| &mut node.content) {
            Some(WindowContent::Leaf(view)) => Ok(view),
            _ => Err(EditorError::InvalidWindow),
        }
    }
    pub fn select(&mut self, id: WindowId) -> Result<()> {
        self.view(id)?;
        self.selected = id;
        Ok(())
    }
    pub fn leaves(&self) -> Vec<WindowId> {
        fn walk(tree: &WindowTree, id: WindowId, out: &mut Vec<WindowId>) {
            match tree.nodes[&id].content {
                WindowContent::Leaf(_) => out.push(id),
                WindowContent::Split { first, second, .. } => {
                    walk(tree, first, out);
                    walk(tree, second, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, self.root, &mut out);
        out
    }
    pub fn select_next(&mut self) -> WindowId {
        let leaves = self.leaves();
        let position = leaves.iter().position(|id| *id == self.selected).unwrap_or(0);
        self.selected = leaves[(position + 1) % leaves.len()];
        self.selected
    }
    fn allocate(&mut self) -> Result<WindowId> {
        Ok(WindowId(crate::allocate_id()?))
    }
    fn replace_link(
        &mut self,
        parent: Option<WindowId>,
        old: WindowId,
        new: WindowId,
    ) -> Result<()> {
        if let Some(parent) = parent {
            match &mut self.nodes.get_mut(&parent).ok_or(EditorError::InvalidWindow)?.content {
                WindowContent::Split { first, .. } if *first == old => *first = new,
                WindowContent::Split { second, .. } if *second == old => *second = new,
                _ => return Err(EditorError::InvalidWindow),
            }
        } else {
            self.root = new;
        }
        Ok(())
    }
    pub fn split(
        &mut self,
        id: WindowId,
        axis: Axis,
        first_cells: Option<u32>,
    ) -> Result<WindowId> {
        // Bound recursive layout/check traversal independently of frame size.
        let mut ancestor = Some(id);
        let mut depth = 0;
        while let Some(current) = ancestor {
            depth += 1;
            if depth >= 128 || self.nodes.len() >= 4095 {
                return Err(EditorError::InvalidGeometry);
            }
            ancestor = self.nodes.get(&current).ok_or(EditorError::InvalidWindow)?.parent;
        }
        let mut staged = self.clone();
        let original = staged.nodes.get(&id).cloned().ok_or(EditorError::InvalidWindow)?;
        let view = staged.view(id)?.clone();
        let extent = match axis {
            Axis::Horizontal => original.rect.height,
            Axis::Vertical => original.rect.width,
        };
        let first_cells = first_cells.unwrap_or(extent / 2);
        if first_cells == 0 || first_cells >= extent {
            return Err(EditorError::InvalidGeometry);
        }
        let parent = staged.allocate()?;
        let new = staged.allocate()?;
        staged.replace_link(original.parent, id, parent)?;
        staged.nodes.get_mut(&id).ok_or(EditorError::InvalidWindow)?.parent = Some(parent);
        staged.nodes.insert(
            new,
            WindowNode {
                id: new,
                parent: Some(parent),
                rect: original.rect,
                content: WindowContent::Leaf(view),
            },
        );
        staged.nodes.insert(
            parent,
            WindowNode {
                id: parent,
                parent: original.parent,
                rect: original.rect,
                content: WindowContent::Split {
                    axis,
                    first: id,
                    second: new,
                    first_units: first_cells,
                    total_units: extent,
                },
            },
        );
        staged.layout()?;
        staged.check_invariants()?;
        *self = staged;
        Ok(new)
    }
    pub fn delete(&mut self, id: WindowId) -> Result<()> {
        self.view(id)?;
        let parent = self.nodes[&id].parent.ok_or(EditorError::LastWindow)?;
        let mut staged = self.clone();
        let parent_node = staged.nodes[&parent].clone();
        let sibling = match parent_node.content {
            WindowContent::Split { first, second, .. } => {
                if first == id {
                    second
                } else {
                    first
                }
            }
            _ => return Err(EditorError::InvalidWindow),
        };
        staged.replace_link(parent_node.parent, parent, sibling)?;
        staged.nodes.get_mut(&sibling).ok_or(EditorError::InvalidWindow)?.parent =
            parent_node.parent;
        staged.nodes.remove(&id);
        staged.nodes.remove(&parent);
        if staged.selected == id {
            staged.selected = staged.first_leaf(sibling);
        }
        staged.layout()?;
        staged.check_invariants()?;
        *self = staged;
        Ok(())
    }
    fn first_leaf(&self, mut id: WindowId) -> WindowId {
        while let WindowContent::Split { first, .. } = self.nodes[&id].content {
            id = first;
        }
        id
    }
    pub fn delete_others(&mut self, keep: WindowId) -> Result<()> {
        self.view(keep)?;
        let mut node = self.nodes[&keep].clone();
        node.parent = None;
        self.nodes.clear();
        self.nodes.insert(keep, node);
        self.root = keep;
        self.selected = keep;
        self.layout()
    }
    /// Entire resize validates before any existing geometry is changed.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let mut staged = self.clone();
        staged.width = width;
        staged.height = height;
        staged.layout()?;
        staged.check_invariants()?;
        *self = staged;
        Ok(())
    }
    fn minimum(&self, id: WindowId) -> Result<(u32, u32)> {
        match self.nodes.get(&id).ok_or(EditorError::InvalidWindow)?.content {
            WindowContent::Leaf(_) => Ok((1, 1)),
            WindowContent::Split { axis, first, second, .. } => {
                let (aw, ah) = self.minimum(first)?;
                let (bw, bh) = self.minimum(second)?;
                match axis {
                    Axis::Horizontal => {
                        Ok((aw.max(bw), ah.checked_add(bh).ok_or(EditorError::InvalidGeometry)?))
                    }
                    Axis::Vertical => {
                        Ok((aw.checked_add(bw).ok_or(EditorError::InvalidGeometry)?, ah.max(bh)))
                    }
                }
            }
        }
    }
    fn layout(&mut self) -> Result<()> {
        fn plan(
            tree: &WindowTree,
            id: WindowId,
            rect: CellRect,
            out: &mut BTreeMap<WindowId, CellRect>,
        ) -> Result<()> {
            let (minw, minh) = tree.minimum(id)?;
            if rect.width < minw || rect.height < minh {
                return Err(EditorError::InvalidGeometry);
            }
            out.insert(id, rect);
            if let WindowContent::Split { axis, first, second, first_units, total_units } =
                tree.nodes[&id].content
            {
                let (aw, ah) = tree.minimum(first)?;
                let (bw, bh) = tree.minimum(second)?;
                let (extent, mina, minb) = match axis {
                    Axis::Horizontal => (rect.height, ah, bh),
                    Axis::Vertical => (rect.width, aw, bw),
                };
                if total_units == 0 || first_units == 0 || first_units >= total_units {
                    return Err(EditorError::InvalidGeometry);
                }
                let size =
                    (u64::from(extent) * u64::from(first_units) / u64::from(total_units)) as u32;
                let size = size.clamp(mina, extent - minb);
                let (a, b) = match axis {
                    Axis::Horizontal => (
                        CellRect { height: size, ..rect },
                        CellRect { y: rect.y + size, height: extent - size, ..rect },
                    ),
                    Axis::Vertical => (
                        CellRect { width: size, ..rect },
                        CellRect { x: rect.x + size, width: extent - size, ..rect },
                    ),
                };
                plan(tree, first, a, out)?;
                plan(tree, second, b, out)?;
            }
            Ok(())
        }
        let mut rectangles = BTreeMap::new();
        plan(
            self,
            self.root,
            CellRect { x: 0, y: 0, width: self.width, height: self.height },
            &mut rectangles,
        )?;
        for (id, rect) in rectangles {
            self.nodes.get_mut(&id).ok_or(EditorError::InvalidWindow)?.rect = rect;
        }
        Ok(())
    }
    pub fn check_invariants(&self) -> Result<()> {
        fn walk(
            tree: &WindowTree,
            id: WindowId,
            parent: Option<WindowId>,
            seen: &mut BTreeSet<WindowId>,
        ) -> Result<()> {
            if !seen.insert(id) {
                return Err(EditorError::InvalidWindow);
            }
            let node = tree.nodes.get(&id).ok_or(EditorError::InvalidWindow)?;
            if node.id != id
                || node.parent != parent
                || node.rect.width == 0
                || node.rect.height == 0
            {
                return Err(EditorError::InvalidGeometry);
            }
            if let WindowContent::Split { axis, first, second, .. } = node.content {
                walk(tree, first, Some(id), seen)?;
                walk(tree, second, Some(id), seen)?;
                let a = tree.nodes[&first].rect;
                let b = tree.nodes[&second].rect;
                let r = node.rect;
                let valid = match axis {
                    Axis::Horizontal => {
                        a.x == r.x
                            && b.x == r.x
                            && a.y == r.y
                            && a.width == r.width
                            && b.width == r.width
                            && a.y.checked_add(a.height) == Some(b.y)
                            && a.height.checked_add(b.height) == Some(r.height)
                    }
                    Axis::Vertical => {
                        a.y == r.y
                            && b.y == r.y
                            && a.x == r.x
                            && a.height == r.height
                            && b.height == r.height
                            && a.x.checked_add(a.width) == Some(b.x)
                            && a.width.checked_add(b.width) == Some(r.width)
                    }
                };
                if !valid {
                    return Err(EditorError::InvalidGeometry);
                }
            }
            Ok(())
        }
        let mut seen = BTreeSet::new();
        walk(self, self.root, None, &mut seen)?;
        if seen.len() != self.nodes.len() {
            return Err(EditorError::InvalidWindow);
        }
        self.view(self.selected)?;
        Ok(())
    }
}
