# Interpreter autoload definition checks

After loading a function's autoload file, interpreter dispatch resolves the
requested name again. An unbound definition signals void-function instead of
unwrapping None. An unchanged autoload stub signals error before evaluating
arguments. A changed stub may load another real file; successful macro loading
re-enters ordinary macro dispatch before argument evaluation.

A 64-load limit rejects continually rewritten stubs. This is an explicit Rune
resource policy, not a GNU limit. Native macro-only autoload-do-load requests,
all keymap autoload paths and complete native/bytecode autoload parity remain
outside this change. Diagnostic wording/file-path reconstruction is not claimed
identical. Global immutable-function clone identity limitations still apply.

Six synthetic real-source cases are checked against GNU: missing/unbound
functions, chained and ordinary definitions, a macro, and propagation of the
file's own signal/partial effects. Canonical function-quoted closures are used;
this does not expand support for legacy raw quoted lambda representations.
Temporary fixtures use exclusive directory creation and clean up their own
function cells/files. No installed package or whole startup success is claimed.
