# Narrow visited-only save-buffer

`(save-buffer)` and `(save-buffer nil)` support Context-owned visited buffer associations and UTF-8 text up to 16 MiB. A non-nil prefix is rejected before filesystem access; backup/version prefix modes are not implemented. Unassociated buffers error without inventing a filename or opening a chooser. Assigning the legacy global buffer-file-name variable does not create an owner association.

The primitive targets the evaluator Env's current buffer, not the selected native window. Native SaveVisited commands select their target explicitly. Both use the same authenticated immutable snapshot, fixed associated path, bounded streaming write and owner completion pipeline. No owner borrow is held during I/O.

An unmodified buffer whose associated target still exists is a no-op; it does not overwrite external content. A missing visited file is recreated, matching GNU's clean-save behavior. Modified buffers use explicit ReplaceExisting policy. The byte budget is checked before writing.

Only successful owner completion marks saved. Text epoch/lineage and association revision must still match the prepared snapshot. A post-write checkpoint failure is reported as file snapshot written but checkpoint rejected, never success and never a reason to replay the I/O automatically. A real successful save advances save generation, not text epoch/history; Undo becomes dirty and Redo to saved content becomes clean.

## Explicit limitations

This is not general Emacs save-buffer parity: no file chooser, full buffer-local variable forwarding, coding detection, file handlers, hooks, backups, lockfiles, external-modification prompts/detection, directory-fsync crash durability or hostile-filesystem race-proofing. In particular, a dirty buffer may replace an externally changed file because external-change tracking is not implemented. Existing backend restrictions on symlinks/nonregular/read-only targets apply. Raw/non-scalar Emacs text remains outside the Unicode model.

Qualification: actual Lisp save targets Env current while a different UI buffer remains dirty/unchanged; saved bytes are exact UTF-8; epoch stays stable; native Undo/Redo observes real save generation. Additional tests cover unassociated/prefix/budget/I/O failures without clearing dirty state, and clean existing external-content preservation versus missing-file recreation. Three tests pass in the text lane with visited-owner and generator dependencies.
