# Captured shell-file-name

For the supported Unix startup, `shell-file-name` is a real owner Lisp variable
initialized from native `SHELL` captured when the Env is created. An absent
variable selects `/bin/sh`; an empty, relative or non-UTF8 value is retained,
not silently replaced. `ESHELL` does not override `SHELL`. Valid UTF8 becomes a
text String; native invalid UTF8 bytes become a ByteString. The immutable native
capture is initialization input, not a second mutable variable authority. Later
Lisp assignments and dynamic bindings use the ordinary Env variable.

No global environment is mutated. Reading this variable neither runs a shell
nor authorizes a command. Existing subprocess limits, complete child-environment
snapshot and absolute-program requirement still apply: preserving a relative or
raw-byte startup value does not imply that every such shell can yet be invoked.
This does not provide Windows COMSPEC selection or complete shell package parity.

Original `subr.el` owns `call-process-shell-command`, and original `simple.el`
owns `shell-command-switch`, `shell-command` and `shell-command-to-string`.
This patch does not replace these functions or fabricate subprocess output.
The original simple.el loading/mark/read-only prerequisites remain separate
from the native variable needed by the real mu4e version check.

Tests use isolated native child test processes for absent, empty, relative,
absolute and raw-byte SHELL cases, avoiding mutation of this threaded process's
environment. GNU30.1 independently gives `/bin/sh` for absent SHELL and preserves
empty/relative/absolute SHELL; ESHELL does not supersede it.
