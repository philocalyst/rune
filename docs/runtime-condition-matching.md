# Shared condition matching

Interpreter and VM now share one callback-free handler matcher. User signal
classes come from that symbol's Env-local error-conditions property. Explicit
NIL metadata differs from absent metadata. Unknown signals with no metadata do
not automatically match their own name or ERROR. T catches all signals, and
handler lists are alternatives. DEBUG is not itself a condition class.

For the minimal runtime before bootstrap, native Err conditions retain their
actual class plus ERROR; explicit ERROR, QUIT and RUNE_UNIMPLEMENTED have builtin
default classes. An explicit property overrides these defaults. This is not full
GNU builtin condition initialization, debugger behavior or named error taxonomy.
Malformed list errors remain generic diagnostics, not precise GNU listp errors.

Matching uses object identity, as GNU memq does, including non-symbol metadata
entries. Membership and handler alternatives short-circuit, so an earlier match does not
inspect a malformed tail. T (including in a handler list) bypasses metadata.
Traversed cdr cycles fail explicitly rather than looping; no compound equality,
Lisp callback or graph rendering occurs. Context is shared for the full scan,
so local Object vectors cannot survive a collection or escape to another owner.
PendingExit/LispError and Env metadata retain their existing roots.

Cycle detection does not bound large acyclic inputs. HashSet allocations are
infallible and no total metadata budget is provided. Frozen QUIT limitations
still apply: no hard timeout, native-subroutine preemption or complete condition
inheritance/debugger compatibility claim.

Handler-list T admission is checked before consulting condition metadata. This
preflight validates the handler spine up to T/end; therefore malformed handler
tails can fail even if an earlier alternative would match metadata. Metadata
membership itself is lazy: an early hit need not inspect a dotted tail. NIL
handlers skip matching without touching metadata. Generic malformed-list
error diagnostics remain a qualification even where rejection ordering matches.
