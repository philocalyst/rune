//! Explicit bounded UTF-8 open boundary; no existing buffer is overwritten.
//! Reads outside owner borrow, publishes text/path/baseline together, but does
//! not select UI or evaluator current-buffer and is not a Lisp file handler.
use super::*;
impl RuntimeEditorGateway {
    pub(crate) fn open_trusted_visited_file(
        &self,
        name: &str,
        path: &Path,
        max_bytes: usize,
        cx: &Context,
    ) -> Result<OwnerBufferHandle> {
        self.check(cx)?;
        ensure!(max_bytes <= 16 * 1024 * 1024, "Visit budget exceeds 16MiB");
        let path = absolute_path(path)?;
        let witness = DiskWitness::capture(&path, max_bytes)?;
        let text = witness.utf8()?; // Missing and invalid UTF-8 fail before mutation.
        let next = revision()?;
        let association = std::sync::Arc::<Path>::from(path.clone().into_boxed_path());
        // One final detection pass before entering the mutation boundary. A
        // concurrent writer can still race afterward; no atomic-open claim.
        witness.verify(&path)?;
        let mut owner = self.inner.try_borrow_mut()?;
        owner.properties.empty(cx)?;
        let document = RuntimeDocument::new(text, &owner.properties, cx)?;
        let id = owner.session.create_visited_buffer(name, document, association)?;
        owner
            .disk_baselines
            .insert(id, Baseline { association: 1, revision: next, witness });
        owner.buffer_handle(id, cx)
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
                "rune-trusted-open-{}-{}",
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
    fn open_publishes_correlated_unicode_root_path_baseline_without_selection() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let before = g.host(&cx).unwrap().snapshot().unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, "λ🙂").unwrap();
        let h = g.open_trusted_visited_file("opened", &p, 64, &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        assert_eq!(read.document.text().len_chars(), 2);
        assert_eq!(read.file.path(), Some(p.as_path()));
        assert!(!g.modified(h, &cx).unwrap());
        use rune_editor::EditorHost;
        assert_eq!(
            g.host(&cx).unwrap().snapshot().unwrap().selected_window,
            before.selected_window
        );
        g.splice(h, read.epoch, 2..2, "!", &cx).unwrap();
        let read = g.read(h, &cx).unwrap();
        let receipt =
            g.prepare_guarded_visited_save(h, read.epoch, 64, &cx).unwrap().write().unwrap();
        g.finish_guarded_save(receipt, &cx).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "λ🙂!");
    }
    #[test]
    fn invalid_open_never_publishes_buffer_or_rebinds_existing() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let g = cx.editor_gateway().unwrap();
        let d = Dir::new();
        let p = d.0.join("file");
        fs::write(&p, [0xff]).unwrap();
        assert!(g.open_trusted_visited_file("bad", &p, 64, &cx).is_err());
        assert!(g.lookup("bad", &cx).unwrap().is_none());
        fs::write(&p, "content").unwrap();
        assert!(g.open_trusted_visited_file("large", &p, 0, &cx).is_err());
        assert!(g.lookup("large", &cx).unwrap().is_none());
        let old = g.create("existing", "owner", &cx).unwrap();
        assert!(g.open_trusted_visited_file("existing", &p, 64, &cx).is_err());
        assert!(g.file_name(old, &cx).unwrap().is_none());
        fs::remove_file(&p).unwrap();
        assert!(g.open_trusted_visited_file("missing", &p, 64, &cx).is_err());
        assert!(g.lookup("missing", &cx).unwrap().is_none());
    }
}
