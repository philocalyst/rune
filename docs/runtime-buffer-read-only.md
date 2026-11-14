# Buffer-read-only forwarding (implementation slice)

`buffer-read-only` reads and dynamic assignments are routed to the Context's
single editor owner for the evaluator's current buffer. The raw Lisp value is
retained by an owning heap-domain root; truthiness controls the native editing
policy. Values are not normalized to `t`, copied into an Env-global current-value
slot, or shared between unrelated buffers. Multiple Envs referencing one buffer
observe the same value.

A dynamic binding records the original native buffer handle and traces the old
raw value. Unwinding restores that original buffer even if the body selected a
different buffer. An authenticated killed-buffer tombstone is skipped, never
replaced by the current buffer. Default/toplevel lookup ignores those per-buffer
binding records. Native bool policy changes canonicalize their value to `t`/nil.

This is not a general buffer-local variable implementation. Makunbound,
DEFCONST, local-variable enumeration, inhibit-read-only text-property semantics,
and default changes affecting newly created buffers require separate coverage.
Fallible dynamic unwind preserves the pending traced binding record when owner
restoration fails (busy, foreign-owner, or sequence exhaustion). Those errors are
propagated, not silently classified as killed buffers. The legacy void wrapper
remains only for callers that explicitly assume restoration cannot fail.
