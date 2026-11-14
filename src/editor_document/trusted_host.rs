//! Opt-in owner host seam, not CLI/default Save wiring.
use super::super::super::LispRuntimeHost;
use super::*;
impl LispRuntimeHost<'_, '_, '_> {
    /// Explicit owner startup/open request. Never replaces an existing buffer.
    /// Selection is an explicit subsequent editor command, not a Lisp binding.
    pub(crate) fn open_selected_trusted_file(
        &mut self,
        name: &str,
        path: &Path,
        max_bytes: usize,
    ) -> Result<(), String> {
        use rune_editor::EditorHost;
        let handle = self
            .owner
            .gateway
            .open_trusted_visited_file(name, path, max_bytes, self.owner.cx)
            .map_err(|e| e.to_string())?;
        self.owner
            .execute(rune_editor::Command::SwitchBuffer(handle.buffer))
            .map_err(|e| format!("File loaded but view selection failed: {e}"))?;
        Ok(())
    }
    /// Guarded typed native Save for explicitly opened files only. Legacy Lisp
    /// save-buffer remains separate and does not gain protection by this API.
    pub(crate) fn save_selected_guarded_visited(
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
            .prepare_guarded_visited_save(handle, read.epoch, max_bytes, self.owner.cx)
            .map_err(|e| format!("Guarded save not started: {e}"))?;
        // Clean existing is the same no-write policy as ordinary Save. Crucially
        // preparing above authenticates a KNOWN baseline, without refreshing it.
        let path = read.file.path().ok_or_else(|| "No visited file".to_owned())?;
        if !gateway.modified(handle, self.owner.cx).map_err(|e| e.to_string())?
            && path.try_exists().map_err(|e| e.to_string())?
        {
            return Ok(SaveOutcome { kind: SaveKind::Unchanged, path: path.to_owned(), bytes: 0 });
        }
        drop(read);
        let receipt = pending
            .write()
            .map_err(|e| format!("Guarded save rejected before commit: {e}"))?;
        gateway.finish_guarded_save(receipt, self.owner.cx).map_err(|e| e.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn explicit_owner_open_then_guarded_save_rejects_external_edits() {
        super::super::super::super::with_lisp_runtime_owner(80, 24, |mut host| {
            assert!(host.save_selected_guarded_visited(64).is_err());
            let p = std::env::temp_dir().join(format!(
                "rune-trusted-host-{}-{}",
                std::process::id(),
                revision()?
            ));
            fs::create_dir(&p)?;
            let dir = Dir(p);
            let path = dir.0.join("file");
            fs::write(&path, "disk")?;
            host.open_selected_trusted_file("opened", &path, 64).unwrap();
            host.evaluate_source(r#"(insert "λ")"#).unwrap();
            let saved = host.save_selected_guarded_visited(64).unwrap();
            assert_eq!(saved.kind, SaveKind::Saved);
            assert_eq!(saved.bytes, 6);
            assert_eq!(fs::read_to_string(&path)?, "λdisk");
            fs::write(&path, "external")?;
            let unchanged = host.save_selected_guarded_visited(64).unwrap();
            assert_eq!(unchanged.kind, SaveKind::Unchanged);
            assert_eq!(unchanged.bytes, 0);
            host.evaluate_source(r#"(insert "!")"#).unwrap();
            assert!(host.save_selected_guarded_visited(64).unwrap_err().contains("before commit"));
            assert_eq!(fs::read_to_string(&path)?, "external");
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn failed_explicit_open_preserves_owner_selection_and_no_buffer_is_published() {
        super::super::super::super::with_lisp_runtime_owner(80, 24, |mut host| {
            use rune_editor::EditorHost;
            let dir = std::env::temp_dir().join(format!(
                "rune-trusted-startup-{}-{}",
                std::process::id(),
                revision()?
            ));
            fs::create_dir(&dir)?;
            let dir = Dir(dir);
            let path = dir.0.join("file");
            let before = host.snapshot()?;
            assert!(host.open_selected_trusted_file("missing", &path, 64).is_err());
            fs::write(&path, [0xff])?;
            assert!(host.open_selected_trusted_file("invalid", &path, 64).is_err());
            fs::write(&path, "too large")?;
            assert!(host.open_selected_trusted_file("over-budget", &path, 0).is_err());
            let after = host.snapshot()?;
            assert_eq!(before.sequence, after.sequence);
            assert_eq!(before.selected_window, after.selected_window);
            assert_eq!(before.buffers.len(), after.buffers.len());
            assert_eq!(before.views[0].view.buffer, after.views[0].view.buffer);
            Ok(())
        })
        .unwrap();
    }
}
