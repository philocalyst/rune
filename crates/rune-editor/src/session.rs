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

/// Read-only native lifetime witness. It retains no buffer, text or GC value.
/// Only the owning Session can close it; a killed identity is never reused.
#[derive(Clone, Debug)]
pub struct BufferLiveness(Arc<std::sync::atomic::AtomicBool>);
impl BufferLiveness {
    fn new() -> Self { Self(Arc::new(std::sync::atomic::AtomicBool::new(true))) }
    pub fn is_live(&self) -> bool { self.0.load(std::sync::atomic::Ordering::Acquire) }
    fn close(&self) { self.0.store(false, std::sync::atomic::Ordering::Release); }
}

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

#[derive(Clone, Copy)]
enum ViewField {
    Point,
    Start,
}
#[derive(Clone)]
struct ViewAdjustment {
    window: WindowId,
    buffer: BufferId,
    field: ViewField,
    offset: usize,
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
    before_views: Option<Vec<ViewAdjustment>>,
    after_views: Option<Vec<ViewAdjustment>>,
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
    liveness: BufferLiveness,
    name: String,
    document: D,
    saved_point: usize,
    read_only: bool,
    version: u64,
    saved_version: u64,
    forced_dirty: bool,
    save_generation: u64,
    file: crate::FileAssociation,
    next_version: u64,
    publication_epoch: u64,
    undo: Option<Arc<History<D>>>,
    redo: Option<Arc<History<D>>>,
}
#[derive(Clone)]
struct DeadBufferMetadata {
    modified: bool,
    file_name: Option<std::sync::Arc<std::path::Path>>,
}
#[derive(Clone)]
struct State<D: Document> {
    buffers: BTreeMap<BufferId, Buffer<D>>,
    // Killed shells retain only this native metadata bit, never text/history,
    // property leases or any GC-owned value.
    dead_buffers: BTreeMap<BufferId, DeadBufferMetadata>,
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
    pub file: crate::FileAssociation,
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
impl<D: Document> Drop for Session<D> {
    fn drop(&mut self) {
        for buffer in self.state.buffers.values() { buffer.liveness.close(); }
    }
}
impl<D: Document> Session<D> {
    pub fn new(document: D, width: u32, height: u32) -> Result<Self> {
        let id = BufferId(crate::allocate_id()?);
        let windows = WindowTree::new(id, width, height)?;
        let buffer = Buffer {
            liveness: BufferLiveness::new(),
            name: "*scratch*".into(),
            document,
            saved_point: 0,
            read_only: false,
            version: 0,
            saved_version: 0,
            forced_dirty: false,
            save_generation: 0,
            file: Default::default(),
            next_version: 1,
            publication_epoch: 0,
            undo: None,
            redo: None,
        };
        let mut session = Self {
            owner: crate::allocate_id()?,
            state: State {
                buffers: [(id, buffer)].into(),
                dead_buffers: BTreeMap::new(),
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
            file: b.file.clone(),
            read_only: b.read_only,
        })
    }
    pub fn buffer_liveness(&self, id: BufferId) -> Result<BufferLiveness> {
        Ok(self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.liveness.clone())
    }
    /// Derive both markers before publishing either. Native IDs consumed by a
    /// failed derivation remain retired; text/history/point/epochs are unchanged.
    pub fn register_marker_pair(&mut self, buffer: BufferId,
        start: usize, start_bias: Bias, end: usize, end_bias: Bias,
    ) -> Result<(crate::MarkerId, crate::MarkerId)> where D: crate::MarkerDocument {
        let document = self.document(buffer)?;
        if start > document.len_chars() || end > document.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        let first = crate::MarkerId(crate::allocate_id()?);
        let second = crate::MarkerId(crate::allocate_id()?);
        if document.marker_position(first.as_u64()).is_some() || document.marker_position(second.as_u64()).is_some() {
            return Err(EditorError::InvalidRange);
        }
        let next = document.set_live_marker(first.as_u64(), start, start_bias)
            .and_then(|doc| doc.set_live_marker(second.as_u64(), end, end_bias))
            .map_err(|e| EditorError::Backend(e.to_string()))?;
        self.state.buffers.get_mut(&buffer).ok_or(EditorError::InvalidBuffer)?.document = next;
        Ok((first, second))
    }
    pub fn move_marker_pair(&mut self, buffer: BufferId,
        pair: (crate::MarkerId, crate::MarkerId), start: usize, start_bias: Bias,
        end: usize, end_bias: Bias,
    ) -> Result<()> where D: crate::MarkerDocument {
        self.transfer_marker_pair(buffer, buffer, pair, start, start_bias, end, end_bias).map(|_| ())
    }
    pub fn transfer_marker_pair(&mut self, source: BufferId, target: BufferId,
        pair: (crate::MarkerId, crate::MarkerId), start: usize, start_bias: Bias,
        end: usize, end_bias: Bias,
    ) -> Result<BufferLiveness> where D: crate::MarkerDocument {
        let old = self.document(source)?;
        if pair.0 == pair.1 || old.marker_position(pair.0.as_u64()).is_none()
            || old.marker_position(pair.1.as_u64()).is_none() {
            return Err(EditorError::InvalidRange);
        }
        let target_doc = self.document(target)?;
        if start > target_doc.len_chars() || end > target_doc.len_chars() {
            return Err(EditorError::InvalidPosition);
        }
        // A different live buffer may not already own these globally unique IDs.
        if source != target && (target_doc.marker_position(pair.0.as_u64()).is_some()
            || target_doc.marker_position(pair.1.as_u64()).is_some()) {
            return Err(EditorError::InvalidRange);
        }
        let next = target_doc.set_live_marker(pair.0.as_u64(), start, start_bias)
            .and_then(|doc| doc.set_live_marker(pair.1.as_u64(), end, end_bias))
            .map_err(|e| EditorError::Backend(e.to_string()))?;
        let old_next = if source != target {
            Some(old.remove_live_marker(pair.0.as_u64()).remove_live_marker(pair.1.as_u64()))
        } else { None };
        if let Some(old_next) = old_next {
            self.state.buffers.get_mut(&source).expect("validated source").document = old_next;
        }
        let target = self.state.buffers.get_mut(&target).expect("validated target");
        target.document = next;
        Ok(target.liveness.clone())
    }
    /// Idempotent pair removal is safe for detached/partially retired overlays.
    pub fn remove_marker_pair(&mut self, buffer: BufferId,
        pair: (crate::MarkerId, crate::MarkerId),
    ) -> Result<()> where D: crate::MarkerDocument {
        let next = self.document(buffer)?.remove_live_marker(pair.0.as_u64())
            .remove_live_marker(pair.1.as_u64());
        self.state.buffers.get_mut(&buffer).expect("validated buffer").document = next;
        Ok(())
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
                liveness: BufferLiveness::new(),
                name: name.into(),
                document,
                saved_point: 0,
                read_only: false,
                version: 0,
                saved_version: 0,
                forced_dirty: false,
                save_generation: 0,
                file: Default::default(),
                next_version: 1,
                publication_epoch: 0,
                undo: None,
                redo: None,
            },
        );
        self.state.mru.push(id);
        Ok(id)
    }
    /// Publish a newly loaded clean document and native association together.
    /// Does not select a UI window or alter evaluator current-buffer binding.
    pub fn create_visited_buffer(
        &mut self,
        name: &str,
        document: D,
        path: std::sync::Arc<std::path::Path>,
    ) -> Result<BufferId> {
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let id = self.create_buffer(name, document)?;
        let buffer = self.state.buffers.get_mut(&id).expect("just created buffer");
        buffer.file = crate::FileAssociation { revision: 1, path: Some(path) };
        self.state.sequence = sequence;
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
        self.complete_save(id, b.publication_epoch, b.file.revision, None).map(|_| ())
    }
    pub fn file_name(&self, id: BufferId) -> Result<Option<std::sync::Arc<std::path::Path>>> {
        if let Some(b) = self.state.buffers.get(&id) {
            return Ok(b.file.path.clone());
        }
        self.state
            .dead_buffers
            .get(&id)
            .map(|b| b.file_name.clone())
            .ok_or(EditorError::InvalidBuffer)
    }
    /// Association changes survive text Undo and invalidate all old save tokens,
    /// including None→path→None ABA. Caller authenticates/normalizes the path.
    pub fn set_file_association(
        &mut self,
        id: BufferId,
        expected_revision: u64,
        path: Option<std::sync::Arc<std::path::Path>>,
    ) -> Result<crate::FileAssociation> {
        let b = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
        if b.file.revision != expected_revision {
            return Err(EditorError::StaleInput);
        }
        let revision = b.file.revision.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let b = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
        b.file = crate::FileAssociation { revision, path };
        self.state.sequence = sequence;
        Ok(b.file.clone())
    }
    /// Commit real save metadata and optional SaveAs binding atomically, after
    /// I/O. No text/history is replaced and no dead shell gains write authority.
    pub fn complete_save(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        expected_file_revision: u64,
        new_path: Option<std::sync::Arc<std::path::Path>>,
    ) -> Result<crate::FileAssociation> {
        let b = self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?;
        if b.publication_epoch != expected_epoch || b.file.revision != expected_file_revision {
            return Err(EditorError::StaleInput);
        }
        let generation = b.save_generation.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let revision = if new_path.is_some() {
            b.file.revision.checked_add(1).ok_or(EditorError::IdExhausted)?
        } else {
            b.file.revision
        };
        let b = self.state.buffers.get_mut(&id).ok_or(EditorError::InvalidBuffer)?;
        b.saved_version = b.version;
        b.forced_dirty = false;
        b.save_generation = generation;
        if let Some(path) = new_path {
            b.file = crate::FileAssociation { revision, path: Some(path) };
        }
        self.state.sequence = sequence;
        Ok(b.file.clone())
    }
    pub fn modified(&self, id: BufferId) -> Result<bool> {
        if let Some(b) = self.state.buffers.get(&id) {
            return Ok(b.forced_dirty || b.version != b.saved_version);
        }
        self.state
            .dead_buffers
            .get(&id)
            .map(|b| b.modified)
            .ok_or(EditorError::InvalidBuffer)
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
    /// Scoped wheel operation: never select/retarget and never move point.
    pub fn scroll_window(
        &mut self,
        window: WindowId,
        buffer: BufferId,
        expected_sequence: u64,
        lines: i64,
    ) -> Result<bool> {
        if self.state.sequence != expected_sequence || self.selected_window() != window {
            return Err(EditorError::StaleInput);
        }
        let view = self.state.windows.view(window)?;
        if view.buffer != buffer {
            return Err(EditorError::StaleInput);
        }
        if lines == 0 {
            return Ok(false);
        }
        let start = self
            .document(buffer)?
            .line_start_offset(view.start, lines)
            .ok_or(EditorError::InvalidPosition)?;
        if start == view.start {
            return Ok(false);
        }
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        self.state.windows.view_mut(window)?.start = start;
        self.state.sequence = sequence;
        Ok(true)
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
        self.map_views_bias(buffer, splice, length, Bias::Left)
    }
    fn map_views_bias(
        &mut self,
        buffer: BufferId,
        splice: Splice,
        length: usize,
        bias: Bias,
    ) -> Result<()> {
        for id in self.state.windows.leaves() {
            let view = self.state.windows.view_mut(id)?;
            if view.buffer == buffer {
                view.point = splice.map(view.point, bias)?.min(length);
                view.start = splice.map(view.start, bias)?.min(length);
            }
        }
        Ok(())
    }
    fn capture_view_undo(
        &self,
        buffer: BufferId,
        range: Range<usize>,
    ) -> Result<Vec<ViewAdjustment>> {
        let mut records = Vec::new();
        for window in self.state.windows.leaves() {
            let view = self.state.windows.view(window)?;
            if view.buffer != buffer {
                continue;
            }
            for (field, position) in
                [(ViewField::Point, view.point), (ViewField::Start, view.start)]
            {
                if position > range.start && position <= range.end {
                    records.push(ViewAdjustment {
                        window,
                        buffer,
                        field,
                        offset: position - range.start,
                    });
                }
            }
        }
        Ok(records)
    }
    fn restore_views(
        &mut self,
        buffer: BufferId,
        splice: Splice,
        length: usize,
        records: Option<&[ViewAdjustment]>,
    ) -> Result<()> {
        let Some(records) = records else { return self.map_views(buffer, splice, length) };
        // Separate phases matter even when the sparse record list is empty.
        let deleted = Splice { start: splice.start, end: splice.end, inserted: 0 };
        self.map_views(
            buffer,
            deleted,
            length.checked_sub(splice.inserted).ok_or(EditorError::InvalidRange)?,
        )?;
        let mut eligible = Vec::new();
        for record in records {
            if record.buffer != buffer || record.offset > splice.inserted {
                continue;
            }
            if let Ok(view) = self.state.windows.view(record.window) {
                let position = match record.field {
                    ViewField::Point => view.point,
                    ViewField::Start => view.start,
                };
                if view.buffer == buffer && position == splice.start {
                    eligible.push(record.clone());
                }
            }
        }
        self.map_views(
            buffer,
            Splice { start: splice.start, end: splice.start, inserted: splice.inserted },
            length,
        )?;
        for record in eligible {
            let position = splice
                .start
                .checked_add(record.offset)
                .ok_or(EditorError::InvalidPosition)?
                .min(length);
            let view = self.state.windows.view_mut(record.window)?;
            match record.field {
                ViewField::Point => view.point = position,
                ViewField::Start => view.start = position,
            }
        }
        Ok(())
    }
    fn edit(&mut self, range: Range<usize>, text: &str) -> Result<BufferId> {
        let id = self.state.windows.view(self.selected_window())?.buffer;
        self.edit_buffer(id, range, text, false, false)
    }
    fn edit_buffer(
        &mut self,
        id: BufferId,
        range: Range<usize>,
        text: &str,
        preserve: bool,
        before_markers: bool,
    ) -> Result<BufferId> {
        self.edit_buffer_prepared(id, range, text, preserve, before_markers, None)
    }
    fn edit_buffer_prepared(
        &mut self, id: BufferId, range: Range<usize>, text: &str,
        preserve: bool, before_markers: bool, prepared: Option<D>,
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
        let before_views = if !coordinate_stable && range.start < range.end {
            Some(self.capture_view_undo(id, range.clone())?)
        } else {
            None
        };
        let document = if let Some(prepared) = prepared {
            let expected = buffer.document.len_chars().checked_sub(range.end-range.start)
                .and_then(|n| n.checked_add(inserted)).ok_or(EditorError::InvalidRange)?;
            if prepared.len_chars() != expected { return Err(EditorError::InvalidRange); }
            // Restore only prepared content; temporary marker registrations made
            // since preparation remain CURRENT and transform through this splice.
            buffer.document.restore_history(&prepared, Some(splice), None)
        } else if before_markers {
            buffer.document.insert_before_markers(range.start, text)
        } else if preserve {
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
            before_views,
            after_views: None,
            before_version: buffer.version,
            after_version: version,
            before_dirty: buffer.forced_dirty || buffer.version != buffer.saved_version,
            after_dirty: true,
            before_dirty_generation: buffer.save_generation,
            after_dirty_generation: buffer.save_generation,
        };
        if !coordinate_stable {
            self.map_views_bias(
                id,
                splice,
                length,
                if before_markers { Bias::Right } else { Bias::Left },
            )?;
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
        let reinserted_views =
            if redo { entry.after_views.clone() } else { entry.before_views.clone() };
        let removed_views =
            if !entry.metadata && !entry.coordinate_stable && splice.start < splice.end {
                Some(self.capture_view_undo(id, splice.start..splice.end)?)
            } else {
                None
            };
        if !entry.metadata && !entry.coordinate_stable {
            if redo {
                entry.before_views = removed_views;
            } else {
                entry.after_views = removed_views;
            }
        }

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
            self.restore_views(id, splice, length, reinserted_views.as_deref())?;
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
        let removed = self.state.buffers.remove(&id).ok_or(EditorError::InvalidBuffer)?;
        self.state
            .dead_buffers
            .insert(id, DeadBufferMetadata { modified, file_name: removed.file.path });
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
                before_views: None,
                after_views: None,
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
    /// File import inserts at current target point, but point stays before the
    /// imported contents. Restore is infallible within the same owner call and
    /// consumes no second sequence allocation after the committed edit.
    pub fn insert_buffer_preserving_point(&mut self,id:BufferId,expected_epoch:u64,at:usize,text:&str)->Result<CommandOutcome> {
        if self.buffer_point(id)?!=at {return Err(EditorError::StaleInput);}
        let outcome=self.replace_buffer(id,expected_epoch,at..at,text)?;
        self.state.buffers.get_mut(&id).expect("validated live buffer").saved_point=at;
        let selected=self.selected_window();
        if self.state.windows.view(selected).expect("live selected view").buffer==id {
            self.state.windows.view_mut(selected).expect("live selected view").point=at;
        }
        Ok(outcome)
    }
    pub fn replace_buffer(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        range: Range<usize>,
        text: &str,
    ) -> Result<CommandOutcome> {
        self.replace_buffer_policy(id, expected_epoch, range, text, false, false)
    }
    pub fn replace_buffer_preserving_properties(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        range: Range<usize>,
        text: &str,
    ) -> Result<CommandOutcome> {
        self.replace_buffer_policy(id, expected_epoch, range, text, true, false)
    }
    pub fn insert_buffer_before_markers(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        at: usize,
        text: &str,
    ) -> Result<CommandOutcome> {
        self.replace_buffer_policy(id, expected_epoch, at..at, text, false, true)
    }
    fn replace_buffer_policy(
        &mut self,
        id: BufferId,
        expected_epoch: u64,
        range: Range<usize>,
        text: &str,
        preserve: bool,
        before_markers: bool,
    ) -> Result<CommandOutcome> {
        self.replace_buffer_prepared_policy(id, expected_epoch, range, text, preserve, before_markers, None)
    }
    /// Commit owner-prepared content in one normal insertion/history transaction.
    /// INTERNAL CROSS-CRATE OWNER BRIDGE: not a validation API for untrusted
    /// documents. Caller must prove prepared is exactly current text with `text`
    /// inserted at `at`, and authenticate every property ID/registry. Only length
    /// and epoch are checked here; arbitrary same-length content is not accepted
    /// as a capability. Public visibility is needed by the runtime binary crate.
    #[doc(hidden)]
    pub fn insert_buffer_prepared(&mut self, id: BufferId, expected_epoch: u64,
        at: usize, text: &str, prepared: D) -> Result<CommandOutcome> {
        self.replace_buffer_prepared_policy(id, expected_epoch, at..at, text, false, false, Some(prepared))
    }
    fn replace_buffer_prepared_policy(&mut self, id: BufferId, expected_epoch: u64,
        range: Range<usize>, text: &str, preserve: bool, before_markers: bool,
        prepared: Option<D>) -> Result<CommandOutcome> {
        if self.state.buffers.get(&id).ok_or(EditorError::InvalidBuffer)?.publication_epoch
            != expected_epoch
        {
            return Err(EditorError::StaleInput);
        }
        let sequence = self.state.sequence.checked_add(1).ok_or(EditorError::IdExhausted)?;
        let backup = self.state.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.edit_buffer_prepared(id, range, text, preserve, before_markers, prepared)
        }));
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
                // No irreversible token transition before the checked command
                // commits. Failed/panicking kills restore the still-live state.
                for (id, buffer) in &backup.buffers {
                    if !self.state.buffers.contains_key(id) { buffer.liveness.close(); }
                }
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
                    file: b.file.clone(),
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
#[cfg(test)]
mod file_association_tests {
    use super::*;
    use crate::StringDocument;
    use std::{path::Path, sync::Arc};
    #[test]
    fn failed_save_cas_or_counter_exhaustion_never_changes_path_or_dirty_state() {
        let mut s = Session::new(StringDocument::empty(), 80, 24).unwrap();
        let b = s.current_buffer();
        s.set_modified(b, true).unwrap();
        let before = s.snapshot().sequence;
        s.state.sequence = u64::MAX;
        assert_eq!(
            s.set_file_association(b, 0, Some(Arc::<Path>::from(Path::new("/native/a")))),
            Err(EditorError::IdExhausted)
        );
        assert!(s.file_name(b).unwrap().is_none());
        assert!(s.modified(b).unwrap());
        s.state.sequence = before;
        s.state.buffers.get_mut(&b).unwrap().file.revision = u64::MAX;
        assert_eq!(
            s.complete_save(b, 0, u64::MAX, Some(Arc::<Path>::from(Path::new("/native/a")))),
            Err(EditorError::IdExhausted)
        );
        assert!(s.file_name(b).unwrap().is_none());
        assert!(s.modified(b).unwrap());
        assert_eq!(s.snapshot().sequence, before);
    }
    #[test]
    fn trusted_open_sequence_exhaustion_or_name_collision_publishes_nothing() {
        let mut s = Session::new(StringDocument::empty(), 80, 24).unwrap();
        let before = s.snapshot();
        s.state.sequence = u64::MAX;
        assert_eq!(
            s.create_visited_buffer(
                "new",
                StringDocument::from("disk"),
                Arc::from(Path::new("/native/new"))
            ),
            Err(EditorError::IdExhausted)
        );
        assert!(s.get_buffer("new").is_none());
        assert_eq!(s.snapshot().buffers.len(), before.buffers.len());
        s.state.sequence = before.sequence;
        let old = s.current_buffer();
        let name = s.snapshot().buffers.iter().find(|b| b.id == old).unwrap().name.clone();
        assert_eq!(
            s.create_visited_buffer(
                &name,
                StringDocument::from("disk"),
                Arc::from(Path::new("/native/new"))
            ),
            Err(EditorError::NameInUse)
        );
        assert_eq!(s.snapshot().sequence, before.sequence);
        assert!(s.file_name(old).unwrap().is_none());
    }
}

#[cfg(test)]
mod overlay_liveness_commit_tests {
    use super::*;
    #[test]
    fn sequence_exhaustion_does_not_retire_a_live_buffer() {
        let mut session = Session::new(crate::StringDocument::from("text"),80,24).unwrap();
        let buffer=session.current_buffer();let token=session.buffer_liveness(buffer).unwrap();
        session.state.sequence=u64::MAX;
        assert_eq!(session.execute(Command::KillBuffer{buffer,force:true}),Err(EditorError::IdExhausted));
        assert!(token.is_live());assert!(session.buffer_live(buffer));
        session.state.sequence=0;
        session.execute(Command::KillBuffer{buffer,force:true}).unwrap();
        assert!(!token.is_live());
    }
}

#[cfg(test)]
mod prepared_insert_failure_tests {
    use super::*;
    use crate::StringDocument;
    #[test]
    fn annotated_prepared_insert_exhaustion_and_readonly_leave_state_unchanged() {
        let mut s=Session::new(StringDocument::from("ab"),80,24).unwrap();
        let id=s.current_buffer();let epoch=s.buffer_snapshot(id).unwrap().version;
        s.state.sequence=u64::MAX;
        assert_eq!(s.insert_buffer_prepared(id,epoch,1,"x",StringDocument::from("axb")),Err(EditorError::IdExhausted));
        assert_eq!(s.buffer_snapshot(id).unwrap().document.to_text(),"ab");
        assert_eq!(s.buffer_snapshot(id).unwrap().version,epoch);
        s.state.sequence=0;s.set_read_only(id,true).unwrap();let sequence=s.snapshot().sequence;
        assert_eq!(s.insert_buffer_prepared(id,epoch,1,"x",StringDocument::from("axb")),Err(EditorError::ReadOnly));
        assert_eq!(s.snapshot().sequence,sequence);assert_eq!(s.buffer_snapshot(id).unwrap().version,epoch);
        assert_eq!(s.buffer_snapshot(id).unwrap().document.to_text(),"ab");
    }
}
