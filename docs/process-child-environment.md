# Child environment snapshots

`start-process` and the supported synchronous `call-process` subset copy the
owner's current `process-environment` into native owned strings before spawning.
The snapshot is a complete replacement, including when the Lisp value is nil.
It does not mutate the runtime's process-wide OS environment. Duplicate variable
names use the first Lisp entry; a bare name suppresses that variable, and `NAME=`
sets an empty value. Unix byte strings preserve native environment bytes. Invalid
keys or NUL bytes are rejected before a child is started.

## Explicit executable-search and directory limitations

GNU searches `exec-path`, independently of a child's `PATH`. Rune does not yet
model that owner search path, so these wrappers require an absolute executable
path. They deliberately reject relative and bare executable names instead of
silently searching an inherited or child-supplied `PATH`. Explicit shell commands
are still possible only by naming an absolute shell executable and passing its
arguments; no shell is introduced by the wrappers.

Subprocess working directories are now copied from the owner `default-directory`
by the scoped local-directory snapshot described in `process-owner-directory.md`.
Automatic buffer-local directory forwarding and full process parity remain
separate obligations.
The official synthetic mu fixture uses an absolute wrapper and explicit isolated
HOME/XDG/muhome paths, so it does not depend on either unsupported behavior.
