# Local special declarations in the current lexical profile

No-initializer defvar records an owner-rooted local declaration and returns its
symbol, without binding NIL or globally marking the symbol special. Existing
lexical cells remain visible: declarations affect later bindings, not already
captured cell identity. New LET bindings explicitly named by captured declarations use the checked
dynamic Env stack. Required, optional and rest lambda parameters remain lexical
cells, shadow captured outer cells, and may escape in closures across GC; this
is true even for names with a local declaration such as integer-width.

Declarations are captured as bare symbols in closure environments, separately
from lexical Cons cells, and restored into rooted declaration slots on call.
Declaration state is local to an Interpreter. A LET installs its declaration
checkpoint only when its first lexical cell is added: parallel initializers run
before that boundary; later serial initializers run within it. Empty/dynamic-only
LET does not establish a new lexical environment. Condition handlers truncate
only their error-variable cells; declarations in body/handler extend the existing
lexical scope. Closures retain declarations after the source scope exits.

The exact interpreter fixture includes an uninterned captured declaration and
matching parameter, forces body GC, verifies its raw address changed, then reads
the lexical parameter after movement and confirms no dynamic binding leaked. Additional
fixtures cover closure GC, shadowing, optional/rest, throw cleanup and initializer
scope. The companion GNU script checks seven lexical behavior cases plus six
scope cases using explicit eval lexical=T.

This does not enable a distinct GNU dynamic eval mode or change loader/native
source defaults. Lambda parameters preserve the existing qualified lexical profile. Initialized
DEFVAR semantics, debugger policy, complete local-special syntax/compilation,
infallible host allocations and Rust panic recovery remain separate work. No
blanket integer-width guard is added to ordinary lexical cells. No new unsafe
lifetime conversion, cross-thread Lisp values or save authority is introduced.

V3 also checks nine GNU lambda/LET interaction cases, including outer-cell
shadowing, escaped parameter closures across GC, optional/rest bindings and
forwarded-name lexical NIL/SETQ. The rejected V2 dynamic-parameter staging is
not part of this increment.
