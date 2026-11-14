# Owner subprocess working directory

`start-process` and `call-process` now copy the owner Lisp `default-directory`
into the native SpawnSpec; they do not inherit the parent's cwd accidentally and
never call host `chdir`. The snapshot uses contextual variable/alias resolution,
so current dynamic bindings apply. Native Unix bytes are preserved, not decoded
lossily. Storage is checked for a 64 KiB byte limit and NUL before PathBuf copy.
Relative, missing and non-directory targets fail closed before process spawn or
creation of an output buffer by these wrappers. This earlier preflight is an
intentional side-effect-ordering deviation: GNU30.1 `start-process` with a
missing working directory creates a requested named output buffer before
signaling `file-missing`; these wrappers reject before creating that buffer.
Do not infer GNU-equivalent failed-call buffer side effects from valid-cwd
results. Directory metadata is a
point-in-time preflight, not a filesystem TOCTOU guarantee; the child current_dir
setup remains fallible. Some later spawn/setup failures may leave a newly created
output buffer as before. No child-output text is published on call-process error.

The native owned path survives moving GC and is the only directory state given
to workers. The existing complete child environment and absolute-program rules
are unchanged. This supplies a genuine requirement of mu4e's original version
check, which binds default-directory to temporary-file-directory.

Scope: local native paths only, not file-name handlers, remote transports, tilde
expansion, narrowing or automatic buffer-local default-directory forwarding.
Invalid types signal structured stringp without printing arbitrary graphs;
relative/path/NUL/budget errors currently use explicit native errors rather than
complete GNU file-missing/file-error condition taxonomy. Metadata/filesystem calls
are not a hard wall-clock bound. Synchronous output still uses its existing UTF8
publication restriction; preserving a raw native cwd does not change that coding
scope. No fabricated version output, copied mu4e handler or package completion
is implied.
