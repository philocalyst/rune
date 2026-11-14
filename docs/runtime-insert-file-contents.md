# Bounded insert-file-contents slice

The actual Lisp primitive accepts FILENAME with nil/omitted VISIT, BEG, END and REPLACE. It reads a regular local file as strict UTF-8, limited to 16 MiB, before publishing any buffer edit. Non-nil optional modes reject explicitly; this is not full Emacs file visiting/coding behavior.

Relative names resolve through the current implementation of default-directory, then a fixed absolute native path. The returned two-element list is (absolute-filename inserted-character-count), with Unicode scalar count rather than byte count. Current point remains at the insertion start. Nonempty insertion is one owner epoch/point-checked transaction and one undo edit; other markers/views follow ordinary insertion policy. Empty files are no-ops.

Read errors, directories, missing files, invalid UTF-8, oversized input, unsupported modes and owner read-only rejection leave buffer text/point/epoch unchanged. The return list/name is allocated before the native edit; no allocation/error-prone point-restoration step occurs after text publication. I/O holds no editor borrow, and borrowed filename/argument data never crosses threads or a mutable Context/GC callback. New file bytes are bounded input, not a whole existing-buffer clone.

Import does not establish a trusted disk baseline, associate a visited file, clear modified state, invoke file handlers/hooks, or implement coding/newline detection. Concurrent external modification is not an atomic-read guarantee. Byte bounds are not a timeout or preemptive I/O cancellation guarantee. Full buffer-local default-directory semantics and optional modes remain future compatibility work.

Three direct integration tests pass: actual Lisp Unicode result/point/Undo, failure/read-only nonmutation, and relative/empty behavior. The atomic insert-preserving-point owner hook is a required dependency. This is aimed at the real observed synthetic Org/mail file-read gate, not evidence that Org, Doom or mu4e packages load.
