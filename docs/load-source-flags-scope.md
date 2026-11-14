# Source load flag scope

The Lisp `load` entry supports FILE, NOERROR, NOMESSAGE, NOSUFFIX and MUST-SUFFIX.
Existing native Rust callers retain their three-argument wrapper. Source search
tries an appended `.el` before the exact name. NOSUFFIX uses the exact name and
takes precedence over MUST-SUFFIX. MUST-SUFFIX disallows a bare unsuffixed library
name, except when FILE already has a recognized suffix or includes a directory.
These precedence rules are qualified against GNU with real fixture files.

NOERROR returns nil for lookup failure, not execution errors or invalid UTF8. GNU-style lookup ignores non-string
load-path entries and non-list/improper tails; cyclic spines stop after the first repeated cell rather than looping. A 100,000
cell traversal limit remains an explicit resource bound. NOMESSAGE suppresses normal load start/end messages. The
prior load-file-name is restored after evaluation errors.

This remains a source-only loader. Compiled Lisp/native modules are explicitly
unsupported, not silently interpreted as source. `.elc` preference, compressed
representations, load-prefer-newer, force-load-messages and complete load-history
semantics are not implemented by this patch. Relative/nil load-path handling
still uses the process directory rather than general buffer-local directories.

Path probing uses Rust is_file and therefore treats metadata/stat failures as
lookup failures, as the previous helper did. This is not a promise to preserve
all GNU permission-error diagnostics or file-name handlers.
