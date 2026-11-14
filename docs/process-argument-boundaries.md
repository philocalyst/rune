# Process argument boundaries

Process wrappers now tag-check invalid Lisp operands before converters can render
arbitrary graphs. Structured wrong-type-argument data retains the original
argument (stringp or processp as GNU requires). start-process's invalid NAME uses
GNU's error message rather than rendering the name. Public NAME/PROGRAM and the
default sentinel's event argument are Objects until these checks run; no generated
&str conversion can format a cyclic object before the wrapper body.

The supported controls preserve GNU's buffer/name/program/argv validation order,
including process-send-string's String-before-process check and the default
filter/sentinel's process-before-String check. Tests compare explicit GNU error
controls and retain cyclic Cons/Vec/ByteFn identity without printing the graph.
This is not a claim of complete filename, process-null, destination, unsupported
mode or failed-call side-effect parity. Existing cwd preflight ordering deviation
remains documented in process-owner-directory.md. In addition, start-process
now validates PROGRAM and argv before creating a requested output buffer. GNU
can create that buffer before reporting a bad argv/program; this wrapper
intentionally leaves it absent on those preflight failures. Matching the tested
type-error predicate/order does not imply matching GNU's buffer-creation effects.

Native copies are bounded before allocation: process names and programs 64 KiB,
each argv string 1 MiB, at most 4096 arguments and 16 MiB total argv. NUL is rejected
before native argv copy. Names/lookup and buffer names require UTF8-representable
text (ASCII ByteString names work); invalid native name bytes fail explicitly,
not lossily. Native Unix PROGRAM/argv bytes remain bytes. The absolute-program
requirement remains; no ambient PATH lookup or NIL-program pipe emulation is
introduced. Send/filter chunks have a 1 MiB per-call bound. These limits are
scoped pre-copy controls, not a universal memory or wall-clock guarantee.

Callback setters, like GNU, retain arbitrary owner-rooted callback objects rather
than eagerly converting them to functions. This change MUST be integrated with
the independent process_events::invoke invalid-tag guard: invalid callback types
signal structured invalid-function on delivery without recursively rendering
cycles. It is not validation/preemption of arbitrary malformed interpreted
function bodies, nor complete filter/sentinel error-policy parity. No Lisp object
is transmitted to native worker threads.
