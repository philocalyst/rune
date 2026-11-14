# Rooted process environment

Generated variable initialization captures the native environment once, in its
original order, into the owning Env's traced `process-environment` Lisp list.
UTF-8 entries become strings; otherwise native encoded bytes are retained in
ByteString. No host environment contents are automatically printed. Subsequent
getenv-internal calls consult this Lisp variable only, including dynamic nil
bindings; there is no live OS fallback.

Lookup supports string/ByteString names, first matching entries, empty values,
and negative entries. A bare name returns nil in the default environment, t in
an explicit nonnil ENV list. Nil ENV uses process-environment. Non-string list
entries are ignored by lookup as in GNU; frames are not implemented.

`process_environment_snapshot(env,cx)` returns a complete owned native vector:
callers must clear inherited environment, then install these pairs. First entry
wins; bare names remove variables. Validation is bounded and rejects cycles,
NUL, empty names and non-string entries before a caller spawns anything. Nil
means an empty child environment, not inheritance. Unix raw bytes are preserved;
non-Unix child snapshots require UTF-8. Windows case-folding and general frame
or coding-system environment behavior are not claimed. Executable resolution
through exec-path is separate from the child's PATH value.
