use crate::{
    Axis, Bias, BufferId, CellRect, Document, EditorError, Result, Splice, View, WindowId,
    WindowTree,
};
use std::{
    collections::{BTreeMap, VecDeque},
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Insert(String),
    ReplaceRange {
        range: Range<usize>,
        text: String,
    },
    DeleteForward(usize),
    DeleteBackward(usize),
    Move(isize),
    GoTo(usize),
    PlaceCaret {
        window: WindowId,
        buffer: BufferId,
        position: usize,
    },
    BeginningOfLine,
    EndOfLine,
    MoveLines(isize),
    FindForward(String),
    FindBackward(String),
    Undo,
    Redo,
    Split(Axis),
    DeleteWindow,
    DeleteOtherWindows,
    NextWindow,
    SelectWindow(WindowId),
    SwitchBuffer(BufferId),
    SwitchToBuffer {
        name: String,
        create: bool,
    },
    KillBuffer {
        buffer: BufferId,
        force: bool,
    },
    RenameBuffer {
        buffer: BufferId,
        name: String,
        unique: bool,
    },
    Resize {
        width: u32,
        height: u32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Metadata,
    Insert,
    Delete,
    Motion,
    Undo,
    Redo,
    Window,
    Buffer,
    Resize,
}
impl Command {
    pub fn kind(&self) -> CommandKind {
        match self {
            Self::Insert(_) | Self::ReplaceRange { .. } => CommandKind::Insert,
            Self::DeleteForward(_) | Self::DeleteBackward(_) => CommandKind::Delete,
            Self::Move(_)
            | Self::GoTo(_)
            | Self::BeginningOfLine
            | Self::EndOfLine
            | Self::MoveLines(_)
            | Self::FindForward(_)
            | Self::FindBackward(_) => CommandKind::Motion,
            Self::Undo => CommandKind::Undo,
            Self::Redo => CommandKind::Redo,
            Self::Resize { .. } => CommandKind::Resize,
            Self::SwitchBuffer(_)
            | Self::SwitchToBuffer { .. }
            | Self::KillBuffer { .. }
            | Self::RenameBuffer { .. } => CommandKind::Buffer,
            _ => CommandKind::Window,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Char(char),
    Control(char),
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Delete,
    Enter,
}
#[derive(Clone, Debug)]
pub enum InputEvent {
    /// Committed IME/paste text, distinct from a physical or control key.
    Text(String),
    /// Native frontend commitment captures its originating focused window.
    TextForWindow {
        window: WindowId,
        buffer: BufferId,
        text: String,
    },
    Key(Key),
    Command(Command),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub sequence: u64,
    pub kind: CommandKind,
    pub changed_buffers: Vec<BufferId>,
    pub selected_window: WindowId,
    pub current_buffer: BufferId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Pending(Vec<Key>),
    Executed(CommandOutcome),
}

#[derive(Clone)]
struct HistoryEntry<D: Document> {
    before: D,
    after: D,
    splice: Splice,
    metadata: bool,
    coordinate_stable: bool,
    before_markers: Option<D::MarkerUndo>,
    after_markers: Option<D::MarkerUndo>,
    before_version: u64,
    after_version: u64,
    before_dirty: bool,
    after_dirty: bool,
    before_dirty_generation: u64,
    after_dirty_generation: u64,
}
#[derive(Clone)]
struct History<D: Document> {
    entry: HistoryEntry<D>,
    next: Option<Arc<History<D>>>,
}
impl<D: Document> Drop for History<D> {
    fn drop(&mut self) {
        let mut next = self.next.take();
        while let Some(link) = next {
            match Arc::try_unwrap(link) {
                Ok(mut node) => next = node.next.take(),
                Err(_) => break,
            }
        }
    }
}
#[derive(Clone)]
struct Buffer<D: Document> {
    name: String,
    document: D,
    saved_point: usize,
    read_only: bool,
    version: u64,
    saved_version: u64,
    forced_dirty: bool,
    save_generation: u64,
    next_version: u64,
    publication_epoch: u64,
    undo: Option<Arc<History<D>>>,
    redo: Option<Arc<History<D>>>,
}
#[derive(Clone)]
struct State<D: Document> {
    buffers: BTreeMap<BufferId, Buffer<D>>,
    // Killed shells retain only this native metadata bit, never text/history,
    // property leases or any GC-owned value.
    dead_modified: BTreeMap<BufferId, bool>,
    mru: Vec<BufferId>,
    current: BufferId,
    windows: WindowTree,
    sequence: u64,
    last_command: Option<CommandKind>,
    last_error: Option<EditorError>,
}
#[derive(Clone)]
pub struct WindowConfiguration {
    owner: u64,
    windows: WindowTree,
    current: BufferId,
}

#[derive(Clone)]
pub struct BufferSnapshot<D> {
    pub id: BufferId,
    pub name: String,
    pub document: D,
    /// Monotonic publication epoch, independent of undo/savepoint content identity.
    pub version: u64,
    pub modified: bool,
    pub read_only: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewSnapshot {
    pub id: WindowId,
    pub rect: CellRect,
    pub view: View,
}
#[derive(Clone)]
pub struct SessionSnapshot<D> {
    pub buffers: Vec<BufferSnapshot<D>>,
    pub views: Vec<ViewSnapshot>,
    pub selected_window: WindowId,
    pub current_buffer: BufferId,
    pub sequence: u64,
}

/// Owner-thread headless editor state. Callbacks into Lisp belong outside this
/// object: submit a typed command and consume immutable snapshots afterward.
/// No document/window guard is held across an external runtime evaluation.
pub struct Session<D: Document> {
    owner: u64,
    state: State<D>,
    bindings: BTreeMap<Vec<Key>, Command>,
    pending: Vec<Key>,
    queue: VecDeque<InputEvent>,
    recording: Option<Vec<Command>>,
    last_macro: Vec<Command>,
    playing_macro: bool,
}
impl<D: Document> Session<D> {
    pub fn new(document: D, width: u32, height: u32) -> Result<Self> {
        let id = BufferId(crate::allocate_id()?);
        let windows = WindowTree::new(id, width, height)?;
        let buffer = Buffer {
            name: "*scratch*".into(),
            document,
            saved_point: 0,
            read_only: false,
            version: 0,
            saved_version: 0,
            forced_dirty: false,
            save_generation: 0,
            next_version: 1,
            publication_epoch: 0,
            undo: None,
            redo: None,
        };
        let mut session = Self {
            owner: crate::allocate_id()?,
            state: State {
                buffers: [(id, buffer)].into(),
                dead_modified: BTreeMap::new(),
                mru: vec![id],
                current: id,
                windows,
                sequence: 0,
                last_command: None,
                last_error: None,
            },
            bindings: BTreeMap::new(),
            pending: Vec::new(),
            queue: VecDeque::new(),
            recording: None,
            last_macro: Vec::new(),
            playing_macro: false,
        };
        let defaults = [
            (vec![Key::Left], Command::Move(-1)),
            (vec![Key::Up], Command::MoveLines(-1)),
            (vec![Key::Down], Command::MoveLines(1)),
            (vec![Key::Right], Command::Move(1)),
            (vec![Key::Home], Command::BeginningOfLine),
            (vec![Key::End], Command::EndOfLine),
            (vec![Key::Backspace], Command::DeleteBackward(1)),
            (vec![Key::Delete], Command::DeleteForward(1)),
            (vec![Key::Enter], Command::Insert("\n".into())),
            (vec![Key::Control('z')], Command::Undo),
            (vec![Key::Control('r')], Command::Redo),
            (vec![Key::Control('x'), Key::Char('2')], Command::Split(Axis::Horizontal)),
            (vec![Key::Control('x'), Key::Char('3')], Command::Split(Axis::Vertical)),
            (vec![Key::Control('x'), Key::Char('0')], Command::DeleteWindow),
            (vec![Key::Control('x'), Key::Char('1')], Command::DeleteOtherWindows),
            (vec![Key::Control('x'), Key::Char('o')], Command::NextWindow),
        ];
        for (keys, command) in defaults {
            session.bind(keys, command)?;
        }
        Ok(session)
    }
    pub fn selected_window(&self) -> WindowId {
        self.state.windows.selected()
    }
    pub fn current_buffer(&self) -> BufferId {
        self.state.current
    }
    pub fn window_tree(&self) -> &WindowTree {
        &self.state.windows
    }
    pub fn point(&self) -> usize {
        self.state.windows.view(self.selected_window()).map_or(0, |view| view.point)
    }
    pub fn current_point(&self) -> Result<usize> {
        let view = self.state.windows.view(self.selected_window())?;
        if view.buffer == self.state.current {
            Ok(view.point)
        } else {
            Ok(self
                .state
                .buffers
                .get(&self.state.current)
                .ok_or(EditorError::InvalidBuffer)?
                .saved_point)
        }
    }
    pub fn set_current_point(&mut self, position: usize) -> Result<()> {
        self.set_buffer_point(self.state.current, position)
    }
    pub fn buffer_point(&self, id: BufferId) -> Result<usize> {
        let view = self.state.windows.view(self.selected_window())?;
        if view.buffer == id {
            Ok(view.point)
        } else {
            Ok(self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.saved_point)
        }
    }
    pub fn set_buffer_point(&mut self, id: BufferId, point: usize) -> Result<()> {
        if point > self.document(id)?.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        if self.buffer_point(id)? == point {
            return Ok(());
        }
        // Direct Lisp/gateway motion is not Session::execute, but must still
        // invalidate captured native scope, including away-and-back ABA.
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        if self.state.windows.view(self.selected_window())?.buffer == id {
            self.move_point(point)?;
        } else {
            self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?.saved_point = point;
        }
        self.state.sequence = sequence;
        Ok(())
    }
    pub fn last_command(&self) -> Option<CommandKind> {
        self.state.last_command
    }
    pub fn last_error(&self) -> Option<&EditorError> {
        self.state.last_error.as_ref()
    }
    pub fn buffer_live(&self, id: BufferId) -> bool {
        self.state.buffers.contains_key(&id)
    }
    pub fn buffer_name(&self, id: BufferId) -> Option<&str> {
        self.state.buffers.get(&id).map(|buffer| buffer.name.as_str())
    }
    pub fn document(&self, id: BufferId) -> Result<&D> {
        Ok(&self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.document)
    }
    pub fn buffer_snapshot(&self, id: BufferId) -> Result<BufferSnapshot<D>> {
        let b = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
        Ok(BufferSnapshot {
            id,
            name: b.name.clone(),
            document: b.document.clone(),
            version: b.publication_epoch,
            modified: b.forced_dirty || b.version != b.saved_version,
            read_only: b.read_only,
        })
    }
    pub fn register_marker(
        &mut self,
        buffer: BufferId,
        position: usize,
        bias: Bias,
    ) -> Result<crate::MarkerId>
    where
        D: crate::MarkerDocument,
    {
        let document = self.document(buffer)?;
        if position > document.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        let marker = crate::MarkerId(crate::allocate_id()?);
        let document = document
            .set_live_marker(marker.as_u64(), position, bias)
            .map_err(|e| EditorError::Backend(e.to_string()))?;
        self.state.buffers.get_mut(&buffer).ok_or(EditorError::InvalidBuffer)?.document = document;
        Ok(marker)
    }
    pub fn marker_position(
        &self,
        buffer: BufferId,
        marker: crate::MarkerId,
    ) -> Result<Option<usize>>
    where
        D: crate::MarkerDocument,
    {
        Ok(self.document(buffer)?.marker_position(marker.as_u64()))
    }
    pub fn remove_marker(&mut self, buffer: BufferId, marker: crate::MarkerId) -> Result<()>
    where
        D: crate::MarkerDocument,
    {
        let document = self.document(buffer)?.remove_live_marker(marker.as_u64());
        self.state.buffers.get_mut(&buffer).ok_or(EditorError::InvalidBuffer)?.document = document;
        Ok(())
    }
    pub fn buffer_list(&self) -> &[BufferId] {
        &self.state.mru
    }
    pub fn get_buffer(&self, name: &str) -> Option<BufferId> {
        self.state.buffers.iter().find_map(|(id, b)| (b.name == name).then_some(*id))
    }
    pub fn create_buffer(&mut self, name: &str, document: D) -> Result<BufferId> {
        if name.is_empty() {
            return Err(EditorError::InvalidName);
        }
        if self.get_buffer(name).is_some() {
            return Err(EditorError::NameInUse);
        }
        let id = BufferId(crate::allocate_id()?);
        self.state.buffers.insert(
            id,
            Buffer {
                name: name.into(),
                document,
                saved_point: 0,
                read_only: false,
                version: 0,
                saved_version: 0,
                forced_dirty: false,
                save_generation: 0,
                next_version: 1,
                publication_epoch: 0,
                undo: None,
                redo: None,
            },
        );
        self.state.mru.push(id);
        Ok(id)
    }
    pub fn set_current_buffer(&mut self, id: BufferId) -> Result<()> {
        self.document(id)?;
        self.state.current = id;
        Ok(())
    }
    pub fn set_read_only(&mut self, id: BufferId, read_only: bool) -> Result<()> {
        let old = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.read_only;
        if old == read_only {
            return Ok(());
        }
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?.read_only = read_only;
        self.state.sequence = sequence;
        Ok(())
    }
    /// A real native savepoint invalidates earlier historical dirty flags.
    /// Explicit Lisp set_modified(false) is different and keeps their validity.
    pub fn mark_saved(&mut self, id: BufferId) -> Result<()> {
        let b = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
        let generation = b.save_generation.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let b = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
        b.saved_version = b.version;
        b.forced_dirty = false;
        b.save_generation = generation;
        self.state.sequence = sequence;
        Ok(())
    }
    pub fn modified(&self, id: BufferId) -> Result<bool> {
        if let Some(b) = self.state.buffers.get(&id) {
            return Ok(b.forced_dirty || b.version != b.saved_version);
        }
        self.state.dead_modified.get(&id).copied().ok_or(EditorError::InvalidBuffer)
    }
    /// Explicit dirty metadata never fabricates text versions or undo entries.
    /// A forced clean baseline remains dirty through text history until saved.
    pub fn set_modified(&mut self, id: BufferId, value: bool) -> Result<()> {
        self.document(id)?; // Dead metadata may be queried, not mutated/adopted.
        if self.modified(id)? == value {
            return Ok(());
        }
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let b = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
        if value {
            b.forced_dirty = true;
        } else {
            b.forced_dirty = false;
            b.saved_version = b.version;
        }
        self.state.sequence = sequence;
        Ok(())
    }
    pub fn set_window_start(&mut self, id: WindowId, start: usize) -> Result<()> {
        let buffer = self.state.windows.view(id)?.buffer;
        if start > self.document(buffer)?.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        if self.state.windows.view(id)?.start == start {
            return Ok(());
        }
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        self.state.windows.view_mut(id)?.start = start;
        self.state.sequence = sequence;
        Ok(())
    }
    fn remember(&mut self, id: BufferId) {
        self.state.mru.retain(|old| *old != id);
        self.state.mru.insert(0, id);
    }
    fn save_selected_point(&mut self) {
        if let Ok(view) = self.state.windows.view(self.selected_window()) {
            if let Some(buffer) = self.state.buffers.get_mut(&view.buffer) {
                buffer.saved_point = view.point;
            }
        }
    }
    fn select(&mut self, id: WindowId) -> Result<()> {
        self.state.windows.view(id)?;
        self.save_selected_point();
        self.state.windows.select(id)?;
        self.state.current = self.state.windows.view(id)?.buffer;
        self.remember(self.state.current);
        Ok(())
    }
    fn switch(&mut self, buffer: BufferId) -> Result<()> {
        let target = self.state.buffers.get(&buffer).ok_or(EditorError::InvalidBuffer)?;
        let point = target.saved_point.min(target.document.len_chars());
        self.save_selected_point();
        let selected = self.selected_window();
        let view = self.state.windows.view_mut(selected)?;
        view.buffer = buffer;
        view.point = point;
        view.start = 0;
        view.hscroll = 0;
        self.state.current = buffer;
        self.remember(buffer);
        Ok(())
    }
    fn move_point(&mut self, position: usize) -> Result<()> {
        let selected = self.selected_window();
        let id = self.state.windows.view(selected)?.buffer;
        if position > self.document(id)?.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        self.state.windows.view_mut(selected)?.point = position;
        self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?.saved_point = position;
        Ok(())
    }
    fn map_views(&mut self, buffer: BufferId, splice: Splice, length: usize) -> Result<()> {
        for id in self.state.windows.leaves() {
            let view = self.state.windows.view_mut(id)?;
            if view.buffer == buffer {
                view.point = splice.map(view.point, Bias::Left)?.min(length);
                view.start = splice.map(view.start, Bias::Left)?.min(length);
            }
        }
        Ok(())
    }
    fn edit(&mut self, range: Range<usize>, text: &str) -> Result<BufferId> {
        let id = self.state.windows.view(self.selected_window())?.buffer;
        self.edit_buffer(id, range, text, false)
    }
    fn edit_buffer(
        &mut self,
        id: BufferId,
        range: Range<usize>,
        text: &str,
        preserve: bool,
    ) -> Result<BufferId> {
        let buffer = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
        if buffer.read_only {
            return Err(EditorError::ReadOnly);
        }
        if range.start > range.end || range.end > buffer.document.len_chars() {
            return Err(EditorError::InvalidRange);
        }
        let publication_epoch =
            buffer.publication_epoch.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let inserted = text.chars().count();
        let splice = Splice { start: range.start, end: range.end, inserted };
        let coordinate_stable = preserve && inserted == range.end - range.start;
        let records = if !coordinate_stable && range.start < range.end {
            Some(
                buffer
                    .document
                    .capture_marker_undo(range.clone())
                    .map_err(|e| EditorError::Backend(e.to_string()))?,
            )
        } else {
            None
        };
        let document = if preserve {
            buffer.document.replace_preserving_properties(range, text)
        } else {
            buffer.document.replace(range, text)
        }
        .map_err(|error| EditorError::Backend(error.to_string()))?;
        let length = document.len_chars();
        let version = buffer.next_version;
        let next = version.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let entry = HistoryEntry {
            before: buffer.document.clone(),
            after: document.clone(),
            splice,
            metadata: false,
            coordinate_stable,
            before_markers: records,
            after_markers: None,
            before_version: buffer.version,
            after_version: version,
            before_dirty: buffer.forced_dirty || buffer.version != buffer.saved_version,
            after_dirty: true,
            before_dirty_generation: buffer.save_generation,
            after_dirty_generation: buffer.save_generation,
        };
        if !coordinate_stable {
            self.map_views(id, splice, length)?;
        }
        let buffer = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
        if !buffer.name.starts_with(' ') {
            buffer.undo = Some(Arc::new(History { entry, next: buffer.undo.clone() }));
        }
        buffer.redo = None;
        buffer.document = document;
        buffer.version = version;
        buffer.publication_epoch = publication_epoch;
        buffer.next_version = next;
        let point = splice.start.checked_add(inserted).ok_or(EditorError::InvalidPosition)?;
        buffer.saved_point = point;
        if self.state.windows.view(self.selected_window())?.buffer == id {
            self.move_point(point)?;
        }
        Ok(id)
    }
    fn undo_redo(&mut self, redo: bool) -> Result<BufferId> {
        let id = self.state.windows.view(self.selected_window())?.buffer;
        let buffer = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
        if buffer.read_only {
            return Err(EditorError::ReadOnly);
        }
        let history = if redo {
            buffer.redo.clone().ok_or(EditorError::NoRedo)?
        } else {
            buffer.undo.clone().ok_or(EditorError::NoUndo)?
        };
        let publication_epoch =
            buffer.publication_epoch.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let mut entry = history.entry.clone();
        // Record CURRENT reverse-side metadata on every actual undo/redo.
        // Explicit flag changes between cycles must not be overwritten by a
        // static initial savepoint record.
        let (target_dirty, target_generation) = if redo {
            (entry.after_dirty, entry.after_dirty_generation)
        } else {
            (entry.before_dirty, entry.before_dirty_generation)
        };
        let current_dirty = buffer.forced_dirty || buffer.version != buffer.saved_version;
        if redo {
            entry.before_dirty = current_dirty;
            entry.before_dirty_generation = buffer.save_generation;
        } else {
            entry.after_dirty = current_dirty;
            entry.after_dirty_generation = buffer.save_generation;
        }
        let (document, version, splice) = if redo {
            (entry.after.clone(), entry.after_version, entry.splice)
        } else {
            (
                entry.before.clone(),
                entry.before_version,
                Splice {
                    start: entry.splice.start,
                    end: entry
                        .splice
                        .start
                        .checked_add(entry.splice.inserted)
                        .ok_or(EditorError::InvalidRange)?,
                    inserted: entry.splice.end - entry.splice.start,
                },
            )
        };
        let reinserted =
            if redo { entry.after_markers.clone() } else { entry.before_markers.clone() };
        let removed = if !entry.metadata && !entry.coordinate_stable && splice.start < splice.end {
            Some(
                buffer
                    .document
                    .capture_marker_undo(splice.start..splice.end)
                    .map_err(|e| EditorError::Backend(e.to_string()))?,
            )
        } else {
            None
        };
        if !entry.metadata && !entry.coordinate_stable {
            if redo {
                entry.before_markers = removed;
            } else {
                entry.after_markers = removed;
            }
        }
        let document = buffer
            .document
            .restore_history(
                &document,
                if entry.metadata || entry.coordinate_stable { None } else { Some(splice) },
                reinserted.as_ref(),
            )
            .map_err(|e| EditorError::Backend(e.to_string()))?;
        let length = document.len_chars();
        if !entry.metadata && !entry.coordinate_stable {
            self.map_views(id, splice, length)?;
        }
        let metadata = entry.metadata;
        let buffer = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
        if redo {
            buffer.redo = history.next.clone();
            buffer.undo = Some(Arc::new(History { entry, next: buffer.undo.clone() }));
        } else {
            buffer.undo = history.next.clone();
            buffer.redo = Some(Arc::new(History { entry, next: buffer.redo.clone() }));
        }
        buffer.document = document;
        buffer.version = version;
        if target_generation != buffer.save_generation {
            // A savepoint occurred after this side's dirty record. Keep that
            // baseline; a historical clean flag cannot mark different saved
            // content clean merely because undo restored an older root.
            buffer.forced_dirty = false;
        } else if target_dirty {
            buffer.forced_dirty = buffer.version == buffer.saved_version;
        } else {
            buffer.saved_version = buffer.version;
            buffer.forced_dirty = false;
        }
        buffer.publication_epoch = publication_epoch;
        if !metadata {
            self.move_point(
                splice.start.checked_add(splice.inserted).ok_or(EditorError::InvalidPosition)?,
            )?;
        }
        Ok(id)
    }
    fn rename(&mut self, id: BufferId, name: String, unique: bool) -> Result<()> {
        if name.is_empty() {
            return Err(EditorError::InvalidName);
        }
        if self.buffer_name(id).is_none() {
            return Err(EditorError::InvalidBuffer);
        }
        let mut target = name.clone();
        let mut suffix = 2u64;
        while self.get_buffer(&target).is_some_and(|other| other != id) {
            if !unique {
                return Err(EditorError::NameInUse);
            }
            target = format!("{name}<{suffix}>");
            suffix = suffix.checked_add(1).ok_or(EditorError::IdExhausted)?;
        }
        self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?.name = target;
        Ok(())
    }
    fn kill(&mut self, id: BufferId, force: bool) -> Result<()> {
        self.document(id)?;
        let modified = self.modified(id)?;
        if modified && !force {
            return Err(EditorError::ModifiedBuffer);
        }
        let fallback = self.state.mru.iter().copied().find(|other| {
            *other != id && self.buffer_name(*other).is_some_and(|name| !name.starts_with(' '))
        });
        let fallback = match fallback {
            Some(value) => value,
            None => {
                let mut name = "*scratch*".to_string();
                let mut suffix = 2;
                while self.get_buffer(&name).is_some() {
                    name = format!("*scratch*<{suffix}>");
                    suffix += 1;
                }
                self.create_buffer(&name, D::empty())?
            }
        };
        let point = self.state.buffers[&fallback]
            .saved_point
            .min(self.document(fallback)?.len_chars());
        for window in self.state.windows.leaves() {
            let view = self.state.windows.view_mut(window)?;
            if view.buffer == id {
                view.buffer = fallback;
                view.point = point;
                view.start = 0;
                view.hscroll = 0;
            }
        }
        self.state.buffers.remove(&id);
        self.state.dead_modified.insert(id, modified);
        self.state.mru.retain(|old| *old != id);
        if self.state.current == id {
            self.state.current = fallback;
        }
        self.remember(fallback);
        Ok(())
    }
    fn apply(&mut self, command: Command) -> Result<Vec<BufferId>> {
        // Restore the selected window's buffer before every interactive command.
        self.state.current = self.state.windows.view(self.selected_window())?.buffer;
        match command {
            Command::Insert(text) => {
                if text.is_empty() {
                    return Ok(Vec::new());
                }
                let point = self.point();
                return Ok(vec![self.edit(point..point, &text)?]);
            }
            Command::ReplaceRange { range, text } => {
                if range.start > range.end {
                    return Err(EditorError::InvalidRange);
                }
                if range.is_empty() && text.is_empty() {
                    if range.start > self.document(self.state.current)?.len_chars() {
                        return Err(EditorError::InvalidRange);
                    }
                    return Ok(Vec::new());
                }
                return Ok(vec![self.edit(range, &text)?]);
            }
            Command::DeleteForward(count) => {
                let point = self.point();
                let end = point.checked_add(count).ok_or(EditorError::InvalidPosition)?;
                if count == 0 {
                    return Ok(Vec::new());
                }
                return Ok(vec![self.edit(point..end, "")?]);
            }
            Command::DeleteBackward(count) => {
                let point = self.point();
                let start = point.checked_sub(count).ok_or(EditorError::InvalidPosition)?;
                if count == 0 {
                    return Ok(Vec::new());
                }
                return Ok(vec![self.edit(start..point, "")?]);
            }
            Command::Move(delta) => {
                let point =
                    self.point().checked_add_signed(delta).ok_or(EditorError::InvalidPosition)?;
                self.move_point(point)?;
            }
            Command::PlaceCaret { window, buffer, position } => {
                if self.state.windows.view(window)?.buffer != buffer {
                    return Err(EditorError::StaleInput);
                }
                if position > self.document(buffer)?.len_chars() {
                    return Err(EditorError::InvalidPosition);
                }
                self.select(window)?;
                self.move_point(position)?;
            }
            Command::GoTo(point) => self.move_point(point)?,
            Command::BeginningOfLine | Command::EndOfLine => {
                let point = self.point();
                let document = self.document(self.state.current)?;
                let mut target = 0;
                if command == Command::BeginningOfLine {
                    document.visit_chars(0, point, &mut |i, ch| {
                        if ch == '\n' {
                            target = i + 1;
                        }
                        true
                    });
                } else {
                    target = document.len_chars();
                    document.visit_chars(point, document.len_chars() - point, &mut |i, ch| {
                        if ch == '\n' {
                            target = i;
                            false
                        } else {
                            true
                        }
                    });
                }
                self.move_point(target)?;
            }
            Command::FindForward(ref query) | Command::FindBackward(ref query) => {
                if query.is_empty() {
                    return Ok(Vec::new());
                }
                let text = self.document(self.state.current)?.to_text();
                let point = self.point();
                let byte = text.char_indices().nth(point).map_or(text.len(), |(byte, _)| byte);
                let target = if matches!(command, Command::FindForward(_)) {
                    let offset =
                        text[byte..].find(query.as_str()).ok_or(EditorError::SearchFailed)?;
                    point + text[byte..byte + offset].chars().count() + query.chars().count()
                } else {
                    let offset =
                        text[..byte].rfind(query.as_str()).ok_or(EditorError::SearchFailed)?;
                    text[..offset].chars().count()
                };
                self.move_point(target)?;
            }
            Command::MoveLines(delta) => {
                let document = self.document(self.state.current)?;
                let point = self.point();
                let mut starts = vec![0];
                document.visit_chars(0, document.len_chars(), &mut |i, ch| {
                    if ch == '\n' {
                        starts.push(i + 1);
                    }
                    true
                });
                let line = starts.partition_point(|start| *start <= point) - 1;
                let column = point - starts[line];
                let target = line
                    .checked_add_signed(delta)
                    .filter(|line| *line < starts.len())
                    .ok_or(EditorError::InvalidPosition)?;
                let end = starts.get(target + 1).map_or(document.len_chars(), |start| start - 1);
                self.move_point(starts[target] + column.min(end - starts[target]))?;
            }
            Command::Undo => return Ok(vec![self.undo_redo(false)?]),
            Command::Redo => return Ok(vec![self.undo_redo(true)?]),
            Command::Split(axis) => {
                self.state.windows.split(self.selected_window(), axis, None)?;
            }
            Command::DeleteWindow => {
                self.save_selected_point();
                self.state.windows.delete(self.selected_window())?;
                self.select(self.state.windows.selected())?;
            }
            Command::DeleteOtherWindows => {
                self.state.windows.delete_others(self.selected_window())?
            }
            Command::NextWindow => {
                let leaves = self.state.windows.leaves();
                let index = leaves
                    .iter()
                    .position(|id| *id == self.selected_window())
                    .ok_or(EditorError::InvalidWindow)?;
                self.select(leaves[(index + 1) % leaves.len()])?;
            }
            Command::SelectWindow(window) => self.select(window)?,
            Command::SwitchBuffer(buffer) => self.switch(buffer)?,
            Command::SwitchToBuffer { name, create } => {
                let buffer = match self.get_buffer(&name) {
                    Some(buffer) => buffer,
                    None if create => self.create_buffer(&name, D::empty())?,
                    None => return Err(EditorError::InvalidBuffer),
                };
                self.switch(buffer)?;
            }
            Command::KillBuffer { buffer, force } => self.kill(buffer, force)?,
            Command::RenameBuffer { buffer, name, unique } => self.rename(buffer, name, unique)?,
            Command::Resize { width, height } => self.state.windows.resize(width, height)?,
        }
        Ok(Vec::new())
    }
    /// A native command is one atomic owner-thread transaction. Text roots and
    /// undo chains are shared immutable snapshots; errors/panics restore state.
    /// Atomic owner-thread validation of an immutable renderer hit.
    pub fn place_pointer(
        &mut self,
        window: WindowId,
        buffer: BufferId,
        epoch: u64,
        position: usize,
    ) -> Result<CommandOutcome> {
        if self.state.windows.view(window)?.buffer != buffer {
            return Err(EditorError::StaleInput);
        }
        let current = self.state.buffers.get(&buffer).ok_or(EditorError::InvalidBuffer)?;
        if current.publication_epoch != epoch {
            return Err(EditorError::StaleInput);
        }
        if position > current.document.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        // Record semantic intent, not an obsolete transport epoch, in macros.
        let outcome = self.execute(Command::PlaceCaret { window, buffer, position })?;
        self.cancel_prefix();
        Ok(outcome)
    }
    pub fn commit_replacement(
        &mut self,
        window: WindowId,
        buffer: BufferId,
        epoch: u64,
        range: Range<usize>,
        text: String,
    ) -> Result<CommandOutcome> {
        if window != self.selected_window() || self.state.windows.view(window)?.buffer != buffer {
            return Err(EditorError::StaleInput);
        }
        if self
            .state
            .buffers
            .get(&buffer)
            .ok_or(EditorError::InvalidBuffer)?
            .publication_epoch
            != epoch
        {
            return Err(EditorError::StaleInput);
        }
        let outcome = self.execute(Command::ReplaceRange { range, text })?;
        self.cancel_prefix();
        Ok(outcome)
    }
    /// Compare-and-swap owner-prepared metadata, preserving CURRENT live markers.
    pub fn publish_metadata(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        document: D,
    ) -> Result<CommandOutcome>
    where
        D: crate::MetadataDocument,
    {
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let backup = self.state.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let b = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
            if b.publication_epoch != expected_epoch {
                return Err(EditorError::StaleInput);
            }
            if b.read_only {
                return Err(EditorError::ReadOnly);
            }
            if !b.document.shares_text_lineage(&document)
                || b.document.len_chars() != document.len_chars()
            {
                return Err(EditorError::InvalidRange);
            }
            let document = b
                .document
                .restore_history(&document, None, None)
                .map_err(|e| EditorError::Backend(e.to_string()))?;
            let epoch = b.publication_epoch.checked_add(1).ok_or(EditorError::IdExhausted)?;
            let version = b.next_version;
            let next = version.checked_add(1).ok_or(EditorError::IdExhausted)?;
            let entry = HistoryEntry {
                before: b.document.clone(),
                after: document.clone(),
                splice: Splice { start: 0, end: 0, inserted: 0 },
                metadata: true,
                coordinate_stable: true,
                before_markers: None,
                after_markers: None,
                before_version: b.version,
                after_version: version,
                before_dirty: b.forced_dirty || b.version != b.saved_version,
                after_dirty: true,
                before_dirty_generation: b.save_generation,
                after_dirty_generation: b.save_generation,
            };
            let b = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
            if !b.name.starts_with(' ') {
                b.undo = Some(Arc::new(History { entry, next: b.undo.clone() }));
            }
            b.redo = None;
            b.document = document;
            b.version = version;
            b.next_version = next;
            b.publication_epoch = epoch;
            self.state.sequence = sequence;
            self.state.last_command = Some(CommandKind::Metadata);
            self.state.last_error = None;
            Ok(CommandOutcome {
                sequence,
                kind: CommandKind::Metadata,
                changed_buffers: vec![id],
                selected_window: self.selected_window(),
                current_buffer: self.state.current,
            })
        }));
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            other => {
                self.state = backup;
                let e = match other {
                    Ok(Err(e)) => e,
                    _ => EditorError::CommandPanicked,
                };
                self.state.last_error = Some(e.clone());
                Err(e)
            }
        }
    }
    /// Runtime edit of current-buffer without selecting/replacing a UI view.
    pub fn replace_current_buffer(
        &mut self,
        range: Range<usize>,
        text: &str,
    ) -> Result<CommandOutcome> {
        let id = self.state.current;
        let epoch =
            self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.publication_epoch;
        self.replace_buffer(id, epoch, range, text)
    }
    pub fn replace_buffer(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        range: Range<usize>,
        text: &str,
    ) -> Result<CommandOutcome> {
        self.replace_buffer_policy(id, expected_epoch, range, text, false)
    }
    pub fn replace_buffer_preserving_properties(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        range: Range<usize>,
        text: &str,
    ) -> Result<CommandOutcome> {
        self.replace_buffer_policy(id, expected_epoch, range, text, true)
    }
    fn replace_buffer_policy(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        range: Range<usize>,
        text: &str,
        preserve: bool,
    ) -> Result<CommandOutcome> {
        if self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.publication_epoch
            != expected_epoch
        {
            return Err(EditorError::StaleInput);
        }
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let backup = self.state.clone();
        let result = catch_unwind(AssertUnwindSafe(|| self.edit_buffer(id, range, text, preserve)));
        match result {
            Ok(Ok(id)) => {
                self.state.sequence = sequence;
                self.state.last_command = Some(CommandKind::Insert);
                self.state.last_error = None;
                Ok(CommandOutcome {
                    sequence,
                    kind: CommandKind::Insert,
                    changed_buffers: vec![id],
                    selected_window: self.selected_window(),
                    current_buffer: self.state.current,
                })
            }
            other => {
                self.state = backup;
                let error = match other {
                    Ok(Err(e)) => e,
                    _ => EditorError::CommandPanicked,
                };
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }
    pub fn execute(&mut self, command: Command) -> Result<CommandOutcome> {
        if !self.playing_macro
            && self.recording.as_ref().is_some_and(|commands| commands.len() >= 10_000)
        {
            return Err(EditorError::MacroTooLarge);
        }
        let next_sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let backup = self.state.clone();
        let kind = command.kind();
        let result = catch_unwind(AssertUnwindSafe(|| self.apply(command.clone())));
        let result = match result {
            Ok(value) => value,
            Err(_) => Err(EditorError::CommandPanicked),
        };
        match result {
            Ok(changed_buffers) => {
                self.state.sequence = next_sequence;
                self.state.last_command = Some(kind);
                self.state.last_error = None;
                if !self.playing_macro {
                    if let Some(recording) = &mut self.recording {
                        recording.push(command);
                    }
                }
                Ok(CommandOutcome {
                    sequence: self.state.sequence,
                    kind,
                    changed_buffers,
                    selected_window: self.selected_window(),
                    current_buffer: self.state.current,
                })
            }
            Err(error) => {
                self.state = backup;
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }
    /// Headless typed bindings, not GNU's Lisp keymap representation. Prefix
    /// ambiguity is rejected rather than depending on an implicit timeout.
    pub fn bind(&mut self, keys: Vec<Key>, command: Command) -> Result<()> {
        if keys.is_empty()
            || self
                .bindings
                .keys()
                .any(|old| old != &keys && (old.starts_with(&keys) || keys.starts_with(old)))
        {
            return Err(EditorError::InvalidBinding);
        }
        self.bindings.insert(keys, command);
        Ok(())
    }
    pub fn dispatch(&mut self, key: Key) -> Result<Dispatch> {
        let was_empty = self.pending.is_empty();
        self.pending.push(key.clone());
        if let Some(command) = self.bindings.get(&self.pending).cloned() {
            self.pending.clear();
            return self.execute(command).map(Dispatch::Executed);
        }
        if self.bindings.keys().any(|keys| keys.starts_with(&self.pending)) {
            return Ok(Dispatch::Pending(self.pending.clone()));
        }
        self.pending.clear();
        if was_empty {
            if let Key::Char(ch) = key {
                return self.execute(Command::Insert(ch.to_string())).map(Dispatch::Executed);
            }
        }
        Err(EditorError::UnboundKey)
    }
    /// Reject stale native commitments instead of inserting into a different
    /// buffer after an intervening focus/window change. Frontend may retry only
    /// after resolving its composition/focus lifecycle explicitly.
    pub fn commit_text(&mut self, window: WindowId, text: String) -> Result<Dispatch> {
        if window != self.selected_window() || self.state.windows.view(window).is_err() {
            return Err(EditorError::StaleInput);
        }
        let buffer = self.state.windows.view(window)?.buffer;
        self.commit_text_for_buffer(window, buffer, text)
    }
    pub fn commit_text_for_buffer(
        &mut self,
        window: WindowId,
        buffer: BufferId,
        text: String,
    ) -> Result<Dispatch> {
        if window != self.selected_window()
            || self.state.windows.view(window).is_err()
            || self.state.windows.view(window)?.buffer != buffer
        {
            return Err(EditorError::StaleInput);
        }
        self.cancel_prefix();
        self.execute(Command::Insert(text)).map(Dispatch::Executed)
    }
    pub fn cancel_prefix(&mut self) {
        self.pending.clear();
    }
    pub fn enqueue(&mut self, event: InputEvent) {
        self.queue.push_back(event);
    }
    pub fn step(&mut self) -> Option<Result<Dispatch>> {
        self.queue.pop_front().map(|event| match event {
            InputEvent::Key(key) => self.dispatch(key),
            InputEvent::Text(text) => self.commit_text(self.selected_window(), text),
            InputEvent::TextForWindow { window, buffer, text } => {
                self.commit_text_for_buffer(window, buffer, text)
            }
            InputEvent::Command(command) => self.execute(command).map(Dispatch::Executed),
        })
    }
    pub fn start_macro(&mut self) -> Result<()> {
        if self.recording.is_some() || self.playing_macro {
            return Err(EditorError::MacroState);
        }
        self.recording = Some(Vec::new());
        Ok(())
    }
    pub fn finish_macro(&mut self) -> Result<usize> {
        self.last_macro = self.recording.take().ok_or(EditorError::MacroState)?;
        Ok(self.last_macro.len())
    }
    pub fn replay_macro(&mut self, repeat: usize) -> Result<Vec<CommandOutcome>> {
        if self.recording.is_some() || self.playing_macro {
            return Err(EditorError::MacroState);
        }
        if self.last_macro.len().checked_mul(repeat).is_none_or(|count| count > 10_000) {
            return Err(EditorError::MacroTooLarge);
        }
        if repeat == 0 || self.last_macro.is_empty() {
            return Ok(Vec::new());
        }
        let commands = self.last_macro.clone();
        self.playing_macro = true;
        let mut outcomes = Vec::new();
        let result = (|| {
            for _ in 0..repeat {
                for command in &commands {
                    outcomes.push(self.execute(command.clone())?);
                }
            }
            Ok(outcomes)
        })();
        self.playing_macro = false;
        result
    }
    pub fn window_configuration(&self) -> WindowConfiguration {
        WindowConfiguration {
            owner: self.owner,
            windows: self.state.windows.clone(),
            current: self.state.current,
        }
    }
    pub fn restore_window_configuration(
        &mut self,
        configuration: &WindowConfiguration,
    ) -> Result<()> {
        if configuration.owner != self.owner {
            return Err(EditorError::ForeignConfiguration);
        }
        let mut windows = configuration.windows.clone();
        let fallback = self.state.windows.view(self.selected_window())?.buffer;
        for id in windows.leaves() {
            let view = windows.view_mut(id)?;
            if !self.buffer_live(view.buffer) {
                view.buffer = fallback;
            }
            let length = self.document(view.buffer)?.len_chars();
            view.point = view.point.min(length);
            view.start = view.start.min(length);
        }
        windows.check_invariants()?;
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        self.state.windows = windows;
        self.state.current = if self.buffer_live(configuration.current) {
            configuration.current
        } else {
            fallback
        };
        self.state.sequence = sequence;
        Ok(())
    }
    pub fn snapshot(&self) -> SessionSnapshot<D> {
        SessionSnapshot {
            buffers: self
                .state
                .buffers
                .iter()
                .map(|(id, b)| BufferSnapshot {
                    id: *id,
                    name: b.name.clone(),
                    document: b.document.clone(),
                    version: b.publication_epoch,
                    modified: b.forced_dirty || b.version != b.saved_version,
                    read_only: b.read_only,
                })
                .collect(),
            views: self
                .state
                .windows
                .leaves()
                .into_iter()
                .filter_map(|id| {
                    self.state.windows.node(id).map(|node| ViewSnapshot {
                        id,
                        rect: node.rect,
                        view: self.state.windows.view(id).expect("leaf invariant").clone(),
                    })
                })
                .collect(),
            selected_window: self.selected_window(),
            current_buffer: self.state.current,
            sequence: self.state.sequence,
        }
    }
    pub fn check_invariants(&self) -> Result<()> {
        self.state.windows.check_invariants()?;
        self.document(self.state.current)?;
        if self.state.mru.len() != self.state.buffers.len() {
            return Err(EditorError::InvalidBuffer);
        }
        let mut mru = self.state.mru.clone();
        mru.sort();
        mru.dedup();
        if mru.len() != self.state.buffers.len() || mru.iter().any(|id| !self.buffer_live(*id)) {
            return Err(EditorError::InvalidBuffer);
        }
        for window in self.state.windows.leaves() {
            let view = self.state.windows.view(window)?;
            let length = self.document(view.buffer)?.len_chars();
            if view.point > length || view.start > length {
                return Err(EditorError::InvalidPosition);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod correctness_tests {
    use super::*;
    use crate::StringDocument;
    #[test]
    fn empty_macro_maximum_repeat_returns_immediately() {
        let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
        session.start_macro().unwrap();
        session.finish_macro().unwrap();
        assert!(session.replay_macro(usize::MAX).unwrap().is_empty());
        assert_eq!(session.snapshot().sequence, 0);
    }
    #[test]
    fn exhausted_sequence_cannot_commit_text() {
        let mut session = Session::new(StringDocument::from("before"), 80, 24).unwrap();
        session.state.sequence = u64::MAX;
        let buffer = session.current_buffer();
        assert_eq!(session.execute(Command::Insert("after".into())), Err(EditorError::IdExhausted));
        assert_eq!(session.document(buffer).unwrap().to_text(), "before");
        assert!(!session.modified(buffer).unwrap());
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    use crate::StringDocument;
    #[test]
    fn exhausted_publication_epoch_rolls_back_edits_and_undo() {
        let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
        let id = session.current_buffer();
        session.execute(Command::Insert("a".into())).unwrap();
        session.state.buffers.get_mut(&id).unwrap().publication_epoch = u64::MAX;
        for command in [Command::Insert("b".into()), Command::Undo] {
            assert_eq!(session.execute(command), Err(EditorError::IdExhausted));
            assert_eq!(session.document(id).unwrap().to_text(), "a");
            assert_eq!(session.state.buffers[&id].version, 1);
        }
    }
}
#[cfg(test)]
mod direct_scope_sequence_tests {
    use super::*;
    use crate::StringDocument;
    #[test]
    fn exhausted_scope_sequence_rejects_direct_ui_mutations_before_change() {
        let mut s = Session::new(StringDocument::from("abc"), 80, 24).unwrap();
        let b = s.current_buffer();
        let w = s.selected_window();
        let config = s.window_configuration();
        s.state.sequence = u64::MAX;
        assert_eq!(s.set_buffer_point(b, 1), Err(EditorError::IdExhausted));
        assert_eq!(s.set_window_start(w, 1), Err(EditorError::IdExhausted));
        assert_eq!(s.set_read_only(b, true), Err(EditorError::IdExhausted));
        assert_eq!(s.restore_window_configuration(&config), Err(EditorError::IdExhausted));
        let view = s.state.windows.view(w).unwrap();
        assert_eq!((view.point, view.start), (0, 0));
        assert!(!s.state.buffers.get(&b).unwrap().read_only);
    }
}
