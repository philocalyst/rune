# Visited filename getter scope

`buffer-file-name` reads the Context-owned per-buffer file association. Its optional argument is a buffer object, not a buffer-name string. Omitted/nil means this Env's current buffer. Unassociated buffers return nil. The getter does not perform filesystem I/O or infer a filename from a buffer's display name.

A successful snapshot write alone does not bind a filename or clear modified state. Native SaveAs completion must validate text epoch/lineage and association revision before publishing the association and real save checkpoint. A failed/stale completion must be reported as such, not replayed as a successful save.

Killed buffer objects retain their last immutable filename for query parity, but no live save authority, document, history or Lisp lease is retained in the path tombstone. Same-name recreated buffers have distinct identities and do not inherit old paths. Buffer renaming does not change its association.

Native paths are lossless. This Lisp Unicode-string getter rejects non-UTF8 paths rather than returning a lossy path that could identify a different file.

## Explicit limitations

This function does not forward the same-named Lisp variable: the existing global `buffer-file-name` variable is not a supported way to set or query owner associations. Full buffer-local variable binding, `set-visited-file-name`, `find-file`, `save-buffer`, coding detection, file handlers, backups, locks and raw-byte Emacs filename semantics are not claimed. Native metadata association and bounded save APIs are separate typed operations.

The build generator must share one canonical symbol between this function and the existing special variable, with independent function/value cells. Renaming a Rust function to hide duplicate Lisp symbols is not a valid substitute.

Qualification: actual reader/evaluator calls across current/noncurrent buffers, SaveAs write-vs-completion boundary, killed-object GC, same-name recreation and rename; Unix non-UTF8 rejection without path loss. Both integration tests pass in the text lane with visited-owner and symbol-dedup dependencies.
