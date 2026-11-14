# write-region source forms

The Lisp write-region entry point accepts three source forms: fixnum START/END scalar buffer positions (reversed endpoints normalized), START=nil for the whole current Unicode buffer, and START=string for literal contents. END is ignored, without type checking, for nil/string START. Numeric START with nil/nonnumeric END rejects before opening the destination. Marker/bignum position arguments and narrowed-buffer semantics are not completed.

Unicode strings are emitted as UTF-8. Existing unibyte ByteString values are written verbatim; this does not add raw-byte editing to Rope buffers or general Emacs coding conversion. Nil sources stream bounded Rope chunks; strings borrow existing contents without a whole-buffer clone. Ordinary writes never move point or clear modified state.

Every source shares one destination-mode implementation: nil APPEND truncates; supported nonnegative fixnum APPEND is a byte seek offset; float offsets reject; truthy nonnumeric APPEND appends. MUSTBENEW='excl is atomic create-new/no-clobber. VISIT, LOCKNAME and confirmation-requiring MUSTBENEW modes remain explicitly unsupported. These rejections and buffer-bound checks precede opening/truncation/creation. I/O errors may still leave partial writes; ordinary write-region is not the atomic temp-write save backend.

The internal numeric-range helper is retained for direct regression tests; the named Lisp wrapper selects source behavior, avoiding duplicated output-mode logic. This rename is not a symbol collision workaround and creates one write-region function symbol.

Qualification: actual Lisp nil/string/ignored-END/raw-byte cases; invalid inputs and modes preserve existing/absent destinations; concurrent string-source exclusive creation yields exactly one complete winner. Local fileio suite:20passed/0failed/1knownignore. Reproducible GNU oracle:5cases passed. No external uploads.
