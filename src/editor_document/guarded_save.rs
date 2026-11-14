//! Opt-in trusted baseline path; legacy explicit snapshot writes are unchanged.
//! Not wired into native/Lisp Save yet. Detection is NOT atomic overwrite CAS.
#[path = "trusted_open.rs"]
mod trusted_open;
use super::disk_witness::DiskWitness;
use super::*;
static NEXT_BASELINE: AtomicU64 = AtomicU64::new(1);
fn revision() -> Result<u64> {
    NEXT_BASELINE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| anyhow::anyhow!("Disk baseline revisions exhausted"))
}
pub(in super::super) struct Baseline {
    association: u64,
    revision: u64,
    witness: DiskWitness,
}
pub(crate) struct GuardedSave {
    pending: PendingSave,
    witness: DiskWitness,
    baseline_revision: u64,
}
pub(crate) struct GuardedWrittenSave {
    written: WrittenSave,
    witness: Result<DiskWitness, String>,
    old_revision: u64,
    next_revision: u64,
}
fn document_bytes(document: &RuntimeDocument) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(document.text().len_bytes());
    for chunk in document.text().chunks(BytePos(0)..BytePos(document.text().len_bytes()))? {
        bytes.extend_from_slice(chunk.as_bytes());
    }
    Ok(bytes)
}
impl RuntimeEditorGateway {
    /// Explicit trusted-read acceptance: require disk bytes equal owner root.
    /// NEVER call automatically to repair Unknown at Save.
    pub(crate) fn establish_visited_baseline(
        &self,
        handle: OwnerBufferHandle,
        epoch: u64,
        max_bytes: usize,
        cx: &Context,
    ) -> Result<()> {
        ensure!(max_bytes <= 16 * 1024 * 1024, "Baseline budget exceeds 16MiB");
        let read = self.read(handle, cx)?;
        ensure!(read.epoch == epoch, "Stale baseline snapshot");
        let path = read.file.path().ok_or_else(|| anyhow::anyhow!("No visited path"))?;
        ensure!(read.document.text().len_bytes() <= max_bytes, "Baseline exceeds byte limit");
        let witness = DiskWitness::capture(path, max_bytes)?;
        ensure!(
            witness.matches_bytes(&document_bytes(&read.document)?),
            "Disk bytes differ from accepted owner snapshot"
        );
        let next = revision()?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(handle, cx)?;
        let now = owner.session.buffer_snapshot(id)?;
        ensure!(
            now.version == epoch
                && now.file == read.file
                && now.document.shares_text_lineage(&read.document),
            "Stale baseline acceptance"
        );
        owner
            .disk_baselines
            .insert(id, Baseline { association: read.file.revision(), revision: next, witness });
        Ok(())
    }
    pub(crate) fn prepare_guarded_visited_save(
        &self,
        handle: OwnerBufferHandle,
        epoch: u64,
        max_bytes: usize,
        cx: &Context,
    ) -> Result<GuardedSave> {
        ensure!(max_bytes <= 16 * 1024 * 1024, "Guarded save budget exceeds 16MiB");
        let pending = self.prepare_visited_save(handle, epoch, max_bytes, cx)?;
        let owner = self.inner.try_borrow()?;
        let id = owner.validate_handle(handle, cx)?;
        let baseline = owner.disk_baselines.get(&id).ok_or_else(|| {
            anyhow::anyhow!("Unknown disk baseline; explicit trusted visit or SaveAs required")
        })?;
        ensure!(
            baseline.association == pending.file_revision,
            "Unknown disk baseline after association change"
        );
        Ok(GuardedSave {
            pending,
            witness: baseline.witness.clone(),
            baseline_revision: baseline.revision,
        })
    }
    pub(crate) fn finish_guarded_save(
        &self,
        receipt: GuardedWrittenSave,
        cx: &Context,
    ) -> Result<SaveOutcome> {
        self.finish_guarded_save_inner(receipt, cx).map_err(|error| error.context(
            "File snapshot written, but checkpoint rejected; do not replay the write"))
    }
    fn finish_guarded_save_inner(&self, receipt:GuardedWrittenSave, cx:&Context)->Result<SaveOutcome> {
        self.check(cx)?;
        let witness = receipt.witness.map_err(|e| {
            anyhow::anyhow!("File snapshot written but witness rejected: {e}; do not replay")
        })?;
        witness.verify(&receipt.written.path)?;
        let mut owner = self.inner.try_borrow_mut()?;
        let id = owner.validate_handle(receipt.written.pending.handle, cx)?;
        let old = owner
            .disk_baselines
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("Written snapshot has retired baseline"))?;
        ensure!(
            old.revision == receipt.old_revision,
            "Written snapshot has stale disk baseline; do not replay"
        );
        let current = owner.session.buffer_snapshot(id)?;
        ensure!(
            current.version == receipt.written.pending.epoch
                && current.document.shares_text_lineage(&receipt.written.pending.document),
            "Written snapshot has stale owner text; do not replay"
        );
        let association = owner.session.complete_save(
            id,
            receipt.written.pending.epoch,
            receipt.written.pending.file_revision,
            None,
        )?;
        owner.disk_baselines.insert(
            id,
            Baseline {
                association: association.revision(),
                revision: receipt.next_revision,
                witness,
            },
        );
        Ok(SaveOutcome { path: receipt.written.path, bytes: receipt.written.pending.bytes })
    }
}
impl GuardedSave {
    pub(crate) fn write(self) -> Result<GuardedWrittenSave> {
        self.write_before_verify(|| {})
    }
    fn write_before_verify(self, before_verify: impl FnOnce()) -> Result<GuardedWrittenSave> {
        let path = self.pending.target.clone().expect("visited pending fixes target");
        self.witness.verify(&path)?;
        let next_revision = revision()?;
        let expected = document_bytes(&self.pending.document)?;
        let written = self.pending.write_with_checks(
            &path,
            SaveMode::ReplaceExisting,
            |p| fs::remove_file(p),
            || {
                before_verify();
                self.witness.verify(&path)?;
                Ok(())
            },
        )?;
        // Output identity comes from the written temp handle, never a fresh
        // pathname adopted as trusted. Always retain a committed receipt.
        let witness = DiskWitness::from_written(expected, &written.committed_metadata);
        let witness = witness.verify(&path).map(|()| witness).map_err(|e| e.to_string());
        Ok(GuardedWrittenSave {
            written,
            witness,
            old_revision: self.baseline_revision,
            next_revision,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gc::RootSet;
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "rune-guarded-{}-{}",
                std::process::id(),
                revision().unwrap()
            ));
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
    fn unknown_baseline_rejects_without_adopting_current_disk() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("unknown", "owner", &cx).unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, "external").unwrap();
        g.set_file_association(h, 0, Some(&p), &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        assert!(g.prepare_guarded_visited_save(h, read.epoch, 64, &cx).is_err());
        assert!(g.establish_visited_baseline(h, read.epoch, 64, &cx).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "external");
    }
    #[test]
    fn trusted_baseline_refresh_conflict_and_association_aba() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("trusted", "old", &cx).unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, "old").unwrap();
        g.set_file_association(h, 0, Some(&p), &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        g.establish_visited_baseline(h, read.epoch, 64, &cx).unwrap();
        g.splice(h, read.epoch, 0..3, "new", &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let pending = g.prepare_guarded_visited_save(h, read.epoch, 64, &cx).unwrap();
        let m = fs::metadata(&p).unwrap().modified().unwrap();
        fs::write(&p, "ext").unwrap();
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(m))
            .unwrap();
        assert!(pending.write().is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "ext");
        assert!(g.modified(h, &cx).unwrap());
        fs::write(&p, "old").unwrap();
        let written =
            g.prepare_guarded_visited_save(h, read.epoch, 64, &cx).unwrap().write().unwrap();
        g.finish_guarded_save(written, &cx).unwrap();
        assert!(!g.modified(h, &cx).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        // Completion refreshes baseline; the next normal edit can save.
        g.splice(h, read.epoch, 0..3, "next", &cx).unwrap();
        let now = g.read(h, &cx).unwrap();
        let written =
            g.prepare_guarded_visited_save(h, now.epoch, 64, &cx).unwrap().write().unwrap();
        g.finish_guarded_save(written, &cx).unwrap();
        let rev = g.read(h, &cx).unwrap().file.revision();
        g.set_file_association(h, rev, None, &cx).unwrap();
        assert!(!g.inner.borrow().disk_baselines.contains_key(&h.buffer));
        g.set_file_association(h, rev + 1, Some(&p), &cx).unwrap();
        assert!(g.prepare_guarded_visited_save(h, now.epoch, 64, &cx).is_err());
    }
    #[test]
    fn new_baseline_rejects_old_committed_receipt_and_kill_drops_bytes() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("cas", "same", &cx).unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, "same").unwrap();
        g.set_file_association(h, 0, Some(&p), &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        g.establish_visited_baseline(h, read.epoch, 64, &cx).unwrap();
        let written =
            g.prepare_guarded_visited_save(h, read.epoch, 64, &cx).unwrap().write().unwrap();
        g.establish_visited_baseline(h, read.epoch, 64, &cx).unwrap();
        assert!(g.finish_guarded_save(written, &cx).unwrap_err().to_string().contains("do not replay"));
        {
            use rune_editor::EditorHost;
            g.host(&cx)
                .unwrap()
                .execute(rune_editor::Command::KillBuffer { buffer: h.buffer, force: true })
                .unwrap();
        }
        assert!(!g.inner.borrow().disk_baselines.contains_key(&h.buffer));
        assert_eq!(g.file_name(h, &cx).unwrap(), Some(p));
    }
    #[test]
    fn last_precommit_check_preserves_external_edit_and_removes_temp() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("precommit", "old", &cx).unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, "old").unwrap();
        g.set_file_association(h, 0, Some(&p), &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        g.establish_visited_baseline(h, read.epoch, 64, &cx).unwrap();
        g.splice(h, read.epoch, 0..3, "new", &cx).unwrap();
        let now = g.read(h, &cx).unwrap();
        let pending = g.prepare_guarded_visited_save(h, now.epoch, 64, &cx).unwrap();
        assert!(
            pending
                .write_before_verify(|| fs::write(&p, "external-after-temp-sync").unwrap())
                .is_err()
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "external-after-temp-sync");
        assert_eq!(fs::read_dir(&d.0).unwrap().count(), 1);
        assert!(g.modified(h, &cx).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn committed_same_bytes_foreign_inode_is_not_adopted_at_finish() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let h = g.create("postcommit", "old", &cx).unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, "old").unwrap();
        g.set_file_association(h, 0, Some(&p), &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        g.establish_visited_baseline(h, read.epoch, 64, &cx).unwrap();
        g.splice(h, read.epoch, 0..3, "new", &cx).unwrap();
        let now = g.read(h, &cx).unwrap();
        let written =
            g.prepare_guarded_visited_save(h, now.epoch, 64, &cx).unwrap().write().unwrap();
        let foreign = d.0.join("foreign");
        fs::write(&foreign, "new").unwrap();
        fs::rename(&foreign, &p).unwrap();
        assert!(g.finish_guarded_save(written, &cx).unwrap_err().to_string().contains("do not replay"));
        assert!(g.modified(h, &cx).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
    }
}
