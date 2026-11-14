//! Bounded UTF-8 snapshot write + atomic owner association/checkpoint.
//! No coding detection, file handlers, backup policy, external-file conflict
//! detection or crash-durable directory commit is implied.
use super::{OwnerBufferHandle, RuntimeDocument, RuntimeEditorGateway};
use crate::core::gc::Context;
use anyhow::{Result, ensure};
use rune_editor::{Document, MetadataDocument};
use rune_text::BytePos;
use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug)]
pub(crate) enum SaveMode {
    ReplaceExisting,
    CreateNew,
}

/// No constructor/import path: capture only an authenticated live owner root.
/// Can move to an I/O worker; it exposes no Lisp values or Context reference.
pub(crate) struct PendingSave {
    handle: OwnerBufferHandle,
    epoch: u64,
    document: RuntimeDocument,
    bytes: usize,
    file_revision: u64,
    target: Option<PathBuf>,
    associate: bool,
}
pub(crate) struct WrittenSave {
    pending: PendingSave,
    path: PathBuf,
}
#[derive(Debug)]
pub(crate) struct SaveOutcome {
    pub(crate) path: PathBuf,
    pub(crate) bytes: usize,
}
struct TempPath(Option<PathBuf>);
impl Drop for TempPath {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}
fn target_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "Save target must be a regular file, not a symlink or directory"
            );
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
impl RuntimeEditorGateway {
    pub(crate) fn prepare_save(
        &self,
        handle: OwnerBufferHandle,
        expected_epoch: u64,
        max_bytes: usize,
        cx: &Context,
    ) -> Result<PendingSave> {
        let read = self.read(handle, cx)?;
        ensure!(read.epoch == expected_epoch, "Stale save snapshot");
        let bytes = read.document.text().len_bytes();
        ensure!(bytes <= max_bytes, "Save exceeds caller byte limit");
        ensure!(
            read.file.path().is_none(),
            "Use visited-save or SaveAs for an associated buffer"
        );
        Ok(PendingSave {
            handle,
            epoch: read.epoch,
            document: read.document,
            bytes,
            file_revision: read.file.revision(),
            target: None,
            associate: false,
        })
    }
    /// File writing already happened. Failure here means "snapshot written,
    /// buffer changed/retired; not marked saved", never permission to replay I/O.
    pub(crate) fn finish_save(&self, written: WrittenSave, cx: &Context) -> Result<SaveOutcome> {
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(written.pending.handle, cx)?;
        let current = owner.session.buffer_snapshot(id)?;
        ensure!(
            current.version == written.pending.epoch
                && current.document.shares_text_lineage(&written.pending.document),
            "Snapshot written, but buffer changed; save checkpoint rejected"
        );
        let association = written
            .pending
            .associate
            .then(|| std::sync::Arc::<Path>::from(written.path.clone().into_boxed_path()));
        owner.session.complete_save(
            id,
            written.pending.epoch,
            written.pending.file_revision,
            association,
        )?;
        Ok(SaveOutcome { path: written.path, bytes: written.pending.bytes })
    }
}
impl PendingSave {
    /// Streams bounded chunks to a same-directory temporary file, flushes it,
    /// then renames. Existing symlink/nonregular targets are unsupported. This
    /// is not a hostile-filesystem race-proof or a file-visit implementation.
    /// On platforms that cannot replace an existing target atomically, rename
    /// fails without explicitly deleting that target.
    pub(crate) fn write_to_path(self, path: &Path, mode: SaveMode) -> Result<WrittenSave> {
        self.write_to_path_with_cleanup(path, mode, |temp| fs::remove_file(temp))
    }

    fn write_to_path_with_cleanup(
        self,
        path: &Path,
        mode: SaveMode,
        remove_temp: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<WrittenSave> {
        let absolute = absolute_path(path)?;
        if let Some(expected) = &self.target {
            ensure!(expected == &absolute, "Save target differs from captured association");
        }
        let path = absolute.as_path();
        let metadata = target_metadata(path)?;
        if matches!(mode, SaveMode::CreateNew) {
            ensure!(metadata.is_none(), "Save target already exists");
        }
        if let Some(metadata) = &metadata {
            ensure!(!metadata.permissions().readonly(), "Save target is read-only");
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Save target needs a file name"))?;
        let mut temporary = None;
        for _ in 0..16 {
            let nonce = NEXT_TEMP
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| anyhow::anyhow!("Save temporary IDs exhausted"))?;
            let mut file_name = std::ffi::OsString::from(".");
            file_name.push(name);
            file_name.push(format!(".rune-save-{}-{nonce}.tmp", std::process::id()));
            let temp = parent.join(file_name);
            match fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => {
                    temporary = Some((temp, file));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let (temp, file) =
            temporary.ok_or_else(|| anyhow::anyhow!("Could not allocate save temporary file"))?;
        let mut cleanup = TempPath(Some(temp.clone()));
        if let Some(metadata) = metadata {
            file.set_permissions(metadata.permissions())?;
        }
        let mut writer = BufWriter::new(file);
        for chunk in self.document.text().chunks(BytePos(0)..BytePos(self.bytes))? {
            writer.write_all(chunk.as_bytes())?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        if let Some(metadata) = target_metadata(path)? {
            ensure!(!metadata.permissions().readonly(), "Save target became read-only");
        }
        match mode {
            SaveMode::ReplaceExisting => fs::rename(&temp, path)?,
            // Atomic no-clobber final-component creation. No explicit target
            // removal or check-then-rename fallback on unsupported platforms.
            SaveMode::CreateNew => {
                fs::hard_link(&temp, path)?;
                // The target is committed once hard_link succeeds. Cleanup
                // failure must not discard the receipt or invite an I/O replay.
                // Leave the guard armed to retry best-effort on scope exit.
                if remove_temp(&temp).is_ok() {
                    cleanup.0 = None;
                }
            }
        }
        if matches!(mode, SaveMode::ReplaceExisting) {
            cleanup.0 = None;
        }
        Ok(WrittenSave { pending: self, path: path.to_owned() })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let p =
                std::env::temp_dir().join(format!("rune-owner-save-{}-{n}", std::process::id()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn streamed_unicode_save_completes_checkpoint_only_after_write_and_preserves_save_scope() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<PendingSave>();
        send_sync::<WrittenSave>();
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let empty = g.read(h, &cx).unwrap();
        g.splice(h, empty.epoch, 0..0, "λ🙂\ntext", &cx).unwrap();
        let before = g.read(h, &cx).unwrap();
        let dir = Directory::new();
        let path = dir.0.join("text");
        fs::write(&path, "old").unwrap();
        let pending = g.prepare_save(h, before.epoch, 100, &cx).unwrap();
        // File I/O holds no gateway borrow and can run while owner access is
        // independently unavailable. Only completion needs the owner again.
        let owner = g.inner.borrow_mut();
        let written = pending.write_to_path(&path, SaveMode::ReplaceExisting).unwrap();
        drop(owner);
        assert!(g.modified(h, &cx).unwrap());
        let saved = g.finish_save(written, &cx).unwrap();
        assert_eq!(saved.path, path);
        assert_eq!(saved.bytes, "λ🙂\ntext".len());
        assert_eq!(fs::read_to_string(&path).unwrap(), "λ🙂\ntext");
        assert!(!g.modified(h, &cx).unwrap());
        assert_eq!(g.read(h, &cx).unwrap().epoch, before.epoch);
        let mut native = g.host(&cx).unwrap();
        native.execute(Command::Undo).unwrap();
        assert!(g.modified(h, &cx).unwrap());
        native.execute(Command::Redo).unwrap();
        assert!(!g.modified(h, &cx).unwrap());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }
    #[test]
    fn stale_completion_keeps_new_text_dirty_and_never_replays_the_write() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let empty = g.read(h, &cx).unwrap();
        g.splice(h, empty.epoch, 0..0, "old", &cx).unwrap();
        let old = g.read(h, &cx).unwrap();
        let pending = g.prepare_save(h, old.epoch, 3, &cx).unwrap();
        g.splice(h, old.epoch, 0..3, "new", &cx).unwrap();
        let dir = Directory::new();
        let path = dir.0.join("text");
        let written = pending.write_to_path(&path, SaveMode::ReplaceExisting).unwrap();
        assert!(g.finish_save(written, &cx).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(g.read(h, &cx).unwrap().document.to_text(), "new");
        assert!(g.modified(h, &cx).unwrap());
    }
    #[test]
    fn limits_and_io_failure_do_not_change_owner_flags_or_touch_existing_target() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let empty = g.read(h, &cx).unwrap();
        g.splice(h, empty.epoch, 0..0, "🙂", &cx).unwrap();
        let before = g.read(h, &cx).unwrap();
        let dir = Directory::new();
        let path = dir.0.join("text");
        fs::write(&path, "unchanged").unwrap();
        assert!(g.prepare_save(h, before.epoch, 3, &cx).is_err());
        assert!(g.prepare_save(h, before.epoch - 1, 100, &cx).is_err());
        let pending = g.prepare_save(h, before.epoch, 4, &cx).unwrap();
        assert!(pending.write_to_path(&dir.0, SaveMode::ReplaceExisting).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "unchanged");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        assert!(g.modified(h, &cx).unwrap());
        assert_eq!(g.read(h, &cx).unwrap().epoch, before.epoch);
    }
    #[cfg(unix)]
    #[test]
    fn symlink_target_is_rejected_without_replacing_link_or_referent() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let dir = Directory::new();
        let target = dir.0.join("target");
        let link = dir.0.join("link");
        fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            g.prepare_save(h, read.epoch, 0, &cx)
                .unwrap()
                .write_to_path(&link, SaveMode::ReplaceExisting)
                .is_err()
        );
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
    }
}
#[cfg(test)]
mod no_clobber_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn create_new_mode_never_replaces_existing_or_dangling_target() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rune-save-new-{}-{nonce}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let existing = dir.join("existing");
        fs::write(&existing, "keep").unwrap();
        assert!(
            g.prepare_save(h, read.epoch, 0, &cx)
                .unwrap()
                .write_to_path(&existing, SaveMode::CreateNew)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&existing).unwrap(), "keep");
        let absent = dir.join("new");
        let written = g
            .prepare_save(h, read.epoch, 0, &cx)
            .unwrap()
            .write_to_path(&absent, SaveMode::CreateNew)
            .unwrap();
        assert!(absent.is_file());
        g.finish_save(written, &cx).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        #[cfg(unix)]
        {
            let referent = dir.join("missing");
            let link = dir.join("link");
            std::os::unix::fs::symlink(&referent, &link).unwrap();
            assert!(
                g.prepare_save(h, read.epoch, 0, &cx)
                    .unwrap()
                    .write_to_path(&link, SaveMode::CreateNew)
                    .is_err()
            );
            assert!(!referent.exists());
            assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        }
    }
}
fn absolute_path(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}
impl RuntimeEditorGateway {
    pub(crate) fn file_name(
        &self,
        handle: OwnerBufferHandle,
        cx: &Context,
    ) -> Result<Option<PathBuf>> {
        self.check(cx)?;
        let owner = self.inner.try_borrow()?;
        owner.properties.empty(cx)?;
        ensure!(handle.owner == cx.domain_id(), "Foreign editor owner");
        Ok(owner.session.file_name(handle.buffer)?.map(|path| path.to_path_buf()))
    }
    /// Owner association metadata only; no disk write/dirty reset. Text Undo
    /// never restores an older path. Lisp setter semantics are not implied.
    pub(crate) fn set_file_association(
        &self,
        handle: OwnerBufferHandle,
        expected_revision: u64,
        path: Option<&Path>,
        cx: &Context,
    ) -> Result<rune_editor::FileAssociation> {
        let path = path
            .map(absolute_path)
            .transpose()?
            .map(|p| std::sync::Arc::<Path>::from(p.into_boxed_path()));
        self.check(cx)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        Ok(owner.session.set_file_association(id, expected_revision, path)?)
    }
    pub(crate) fn prepare_visited_save(
        &self,
        handle: OwnerBufferHandle,
        expected_epoch: u64,
        max_bytes: usize,
        cx: &Context,
    ) -> Result<PendingSave> {
        let read = self.read(handle, cx)?;
        ensure!(read.epoch == expected_epoch, "Stale save snapshot");
        let bytes = read.document.text().len_bytes();
        ensure!(bytes <= max_bytes, "Save exceeds caller byte limit");
        let target = read
            .file
            .path()
            .ok_or_else(|| anyhow::anyhow!("Buffer has no visited file"))?
            .to_path_buf();
        Ok(PendingSave {
            handle,
            epoch: read.epoch,
            document: read.document,
            bytes,
            file_revision: read.file.revision(),
            target: Some(target),
            associate: false,
        })
    }
    pub(crate) fn prepare_save_as(
        &self,
        handle: OwnerBufferHandle,
        expected_epoch: u64,
        path: &Path,
        max_bytes: usize,
        cx: &Context,
    ) -> Result<PendingSave> {
        let target = absolute_path(path)?;
        let read = self.read(handle, cx)?;
        ensure!(read.epoch == expected_epoch, "Stale save snapshot");
        let bytes = read.document.text().len_bytes();
        ensure!(bytes <= max_bytes, "Save exceeds caller byte limit");
        Ok(PendingSave {
            handle,
            epoch: read.epoch,
            document: read.document,
            bytes,
            file_revision: read.file.revision(),
            target: Some(target),
            associate: true,
        })
    }
}
impl PendingSave {
    /// Write only the captured visited/SaveAs path. Arbitrary retargeting is
    /// rejected by write_to_path before touching the filesystem.
    pub(crate) fn write(self, mode: SaveMode) -> Result<WrittenSave> {
        let path = self
            .target
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Unbound snapshot save needs an explicit path"))?;
        self.write_to_path(&path, mode)
    }
}
#[cfg(test)]
mod visit_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_editor::{Command, EditorHost};
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let p =
                std::env::temp_dir().join(format!("rune-save-visit-{}-{n}", std::process::id()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn save_as_binds_only_after_write_and_checkpoint_undo_never_reverts_path() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let empty = g.read(h, &cx).unwrap();
        g.splice(h, empty.epoch, 0..0, "λ🙂", &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let dir = Dir::new();
        let path = dir.0.join("visited");
        let pending = g.prepare_save_as(h, read.epoch, &path, 100, &cx).unwrap();
        assert_eq!(g.file_name(h, &cx).unwrap(), None);
        let written = pending.write(SaveMode::CreateNew).unwrap();
        assert_eq!(g.file_name(h, &cx).unwrap(), None);
        g.finish_save(written, &cx).unwrap();
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(path.clone()));
        assert!(!g.modified(h, &cx).unwrap());
        assert!(g.prepare_save(h, read.epoch, 100, &cx).is_err());
        let mut native = g.host(&cx).unwrap();
        native.execute(Command::Undo).unwrap();
        assert!(g.modified(h, &cx).unwrap());
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(path.clone()));
        native.execute(Command::Redo).unwrap();
        assert!(!g.modified(h, &cx).unwrap());
        native.execute(Command::KillBuffer { buffer: h.buffer, force: true }).unwrap();
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(path));
        assert!(g.read(h, &cx).is_err());
        assert!(g.prepare_visited_save(h, read.epoch, 100, &cx).is_err());
    }
    #[test]
    fn association_aba_rejects_completion_even_with_same_text_epoch_and_no_path_again() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let dir = Dir::new();
        let target = dir.0.join("as");
        let temporary = dir.0.join("temporary-association");
        let pending = g.prepare_save_as(h, read.epoch, &target, 0, &cx).unwrap();
        let a = g.set_file_association(h, read.file.revision(), Some(&temporary), &cx).unwrap();
        g.set_file_association(h, a.revision(), None, &cx).unwrap();
        let written = pending.write(SaveMode::CreateNew).unwrap();
        assert!(g.finish_save(written, &cx).is_err());
        assert_eq!(g.file_name(h, &cx).unwrap(), None);
        assert_eq!(g.read(h, &cx).unwrap().epoch, read.epoch);
        assert!(target.exists());
    }
    #[test]
    fn visited_save_cannot_retarget_and_stale_save_as_never_rebinds() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.scratch(&cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let dir = Dir::new();
        let old = dir.0.join("old");
        let new = dir.0.join("new");
        let wrong = dir.0.join("wrong");
        let written = g
            .prepare_save_as(h, read.epoch, &old, 0, &cx)
            .unwrap()
            .write(SaveMode::CreateNew)
            .unwrap();
        g.finish_save(written, &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        assert!(
            g.prepare_visited_save(h, read.epoch, 0, &cx)
                .unwrap()
                .write_to_path(&wrong, SaveMode::CreateNew)
                .is_err()
        );
        assert!(!wrong.exists());
        let pending = g.prepare_save_as(h, read.epoch, &new, 0, &cx).unwrap();
        g.splice(h, read.epoch, 0..0, "edited", &cx).unwrap();
        let written = pending.write(SaveMode::CreateNew).unwrap();
        assert!(g.finish_save(written, &cx).is_err());
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(old));
        assert!(g.modified(h, &cx).unwrap());
    }
    #[test]
    fn killed_path_tombstone_cannot_be_adopted_by_new_same_name_or_foreign_owner() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("named", "x", &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let dir = Dir::new();
        let path = dir.0.join("named");
        g.finish_save(
            g.prepare_save_as(h, read.epoch, &path, 1, &cx)
                .unwrap()
                .write(SaveMode::CreateNew)
                .unwrap(),
            &cx,
        )
        .unwrap();
        g.kill(h, true, &cx).unwrap();
        let new = g.create("named", "new", &cx).unwrap();
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(path));
        assert_eq!(g.file_name(new, &cx).unwrap(), None);
        let other = RuntimeEditorGateway::new(&cx, 80, 24).unwrap();
        assert!(other.file_name(h, &cx).is_err());
    }
    #[test]
    fn create_new_cleanup_failure_preserves_committed_receipt() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("cleanup", "λ🙂", &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let dir = Dir::new();
        let path = dir.0.join("committed");
        let written = g
            .prepare_save_as(h, read.epoch, &path, 64, &cx)
            .unwrap()
            .write_to_path_with_cleanup(&path, SaveMode::CreateNew, |temp| {
                assert!(temp.exists());
                assert_eq!(fs::read_to_string(&path).unwrap(), "λ🙂");
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected unlink failure",
                ))
            })
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "λ🙂");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        assert_eq!(g.file_name(h, &cx).unwrap(), None);
        g.finish_save(written, &cx).unwrap();
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(path));
        assert!(!g.modified(h, &cx).unwrap());
    }
}

impl super::LispRuntimeHost<'_, '_, '_> {
    /// Explicit native Save command. Only an already-associated selected UI
    /// buffer is eligible; evaluator-local current-buffer is not consulted.
    /// The caller must never replay this callback after an error or lost reply.
    pub(crate) fn save_selected_visited(
        &mut self,
        max_bytes: usize,
    ) -> Result<SaveOutcome, String> {
        use rune_editor::EditorHost;
        let snapshot = self.owner.snapshot().map_err(|e| e.to_string())?;
        let buffer = snapshot
            .views
            .iter()
            .find(|v| v.id == snapshot.selected_window)
            .ok_or_else(|| "Missing selected editor view".to_owned())?
            .view
            .buffer;
        drop(snapshot);
        let gateway = &self.owner.gateway;
        let handle = gateway
            .inner
            .try_borrow()
            .map_err(|e| e.to_string())?
            .buffer_handle(buffer, self.owner.cx)
            .map_err(|e| e.to_string())?;
        let read = gateway.read(handle, self.owner.cx).map_err(|e| e.to_string())?;
        let pending = gateway
            .prepare_visited_save(handle, read.epoch, max_bytes, self.owner.cx)
            .map_err(|e| format!("Save not started: {e}"))?;
        drop(read);
        let written = pending
            .write(SaveMode::ReplaceExisting)
            .map_err(|e| format!("Save write failed: {e}"))?;
        gateway.finish_save(written, self.owner.cx).map_err(|e| {
            format!(
                "File snapshot written, but buffer checkpoint not accepted: {e}; do not replay save"
            )
        })
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn explicit_save_uses_selected_view_not_evaluator_binding_and_requires_path() {
        super::super::with_lisp_runtime_owner(80, 24, |mut host| {
            use rune_editor::EditorHost;
            let snapshot = host.snapshot()?;
            let buffer = snapshot.views.iter().find(|v| v.id == snapshot.selected_window).unwrap().view.buffer;
            let gateway = host.owner.gateway.clone();
            let handle = gateway.inner.borrow().buffer_handle(buffer, host.owner.cx)?;
            assert!(host.save_selected_visited(64).unwrap_err().contains("Save not started"));
            let dir = std::env::temp_dir().join(format!("rune-save-command-{}-{}",std::process::id(),NEXT_TEMP.fetch_add(1,Ordering::Relaxed)));
            fs::create_dir(&dir)?;
            let cleanup = TempPath(Some(dir.join("selected")));
            gateway.set_file_association(handle, 0, Some(cleanup.0.as_ref().unwrap()), host.owner.cx)?;
            host.evaluate_source(r#"(insert "selected") (set-buffer (get-buffer-create "other-save-target")) (insert "other")"#).unwrap();
            fs::write(cleanup.0.as_ref().unwrap(), "unchanged")?;
            assert!(host.save_selected_visited(0).unwrap_err().contains("Save not started"));
            assert_eq!(fs::read_to_string(cleanup.0.as_ref().unwrap())?, "unchanged");
            assert!(gateway.modified(handle, host.owner.cx)?);
            let outcome = host.save_selected_visited(64).unwrap();
            assert_eq!(fs::read_to_string(&outcome.path)?, "selected");
            assert_eq!(outcome.bytes, 8);
            assert!(!gateway.modified(handle,host.owner.cx)?);
            drop(cleanup);
            fs::remove_dir(dir)?;
            Ok(())
        }).unwrap();
    }
}
