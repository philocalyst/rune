# Startup debug flag scope

GNU Emacs 30.1 startup.el declares `(defvar init-file-debug nil)`. Rune's native
startup driver initializes that same object-valued flag to nil. Lisp can inspect,
set, or dynamically bind it; ordinary startup does not invent a true/debug state.

This does not implement the whole GNU startup driver, an interactive Lisp
debugger, or the GNU `--debug-init` command-line switch. Rune rejects unsupported
CLI switches rather than silently pretending debugger support. Existing bounded
error diagnostics remain available independently of this flag.
