# VM conditions and resource boundaries: follow-up design

Status: follow-up design plus an independently owned pending-exit prototype,
against published `eac483c` (including PushCatch/callable throw and deferred
channel lifecycle). This document and prototype are separate from the frozen
PushCatch patch. Focused tests do not establish GNU compatibility, general
memory safety, or resource isolation. The gaps below describe the baseline;
the prototype removes the single-slot pending-exit dependency only.

## Confirmed current behavior and gaps

- Active catches compare tag identity and preserve the thrown value. Throws pass
  through condition handlers. PushCatch now has frame, handler, binding, operand,
  and catch-stack checkpoints; lazy invalid-opcode handling remains deliberate.
- Unmatched throw becomes generic ERROR plus a rendered diagnostic, not GNU's
  structured `(no-catch TAG VALUE)` condition.
- Interpreter and VM condition matching have separate broad ERROR-oriented
  implementations. Specific conditions, inheritance, and DEBUG semantics are not
  complete. TypeError retains types and an owned rendering, not the original
  offending Lisp object. A string-to-condition translation cannot recover identity.
- LispError keeps its condition list rooted in its originating heap domain.
  Foreign/retired binding deliberately falls back to a fresh ERROR diagnostic.
  Its arity payload currently includes expected and actual counts, unlike GNU's
  function plus actual-count payload. The native-call upfront check preserves
  the called symbol, but does not change this existing payload representation.
- Env has one exception pair and a u32 generation. Throw/Signal errors retain
  only that generation. A pending exit is not independently rooted once another
  exit replaces the pair. `unwind_protect` evaluates cleanup while retaining the
  old error, making this a concrete nested-exit bug, not just generation exhaustion.
- Ordinary CallFrame entry/finalization unwraps usize-to-u16 conversions. More
  than 65,535 evaluated arguments to a variadic function, or a sufficiently large
  apply argument list, can panic. Optimized bytecode operand counts are u16;
  valid 7-bit bytecode required/max specs keep optional/rest arithmetic bounded.
- Checked bytecode stack/frame reservations do not bound total work or memory.
  Decoder boundary arrays, instruction arrays, cache keys, handler vectors, and
  several ordinary call paths still allocate infallibly. The per-VM decoded cache
  retains every distinct byte sequence until that VM returns. No instruction,
  host-recursion, or elapsed-time budget currently exists.

## First prerequisite: independently owned pending exits

Before expanding condition matching, remove the single-slot ownership dependency.
A pending Throw or Signal should own a GC-safe payload handle, not an id naming
Env's latest exception. Reuse the audited domain-root abstraction used by
LispError rather than adding untraced Object fields or pretending a lifetime is
static. A payload can root one cons containing `(TAG . VALUE)` or
`(CONDITION . DATA)`; binding in its owner domain must preserve identity.
Constructors need the owner Context, so update signal/throw producers and both
consumers coherently. Nonlocal catch identity must not be reconstructed from
rendered strings or silently transferred to another heap domain. Define a
foreign/retired pending-exit fallback explicitly, consistent with LispError's
existing diagnostic fallback, before changing cross-thread error reporting.

A narrowly scoped interim alternative is to snapshot/root a pending pair across
`unwind_protect` cleanup and reissue its exit on successful cleanup. It fixes that
one boundary but does not make the single-slot protocol safe in general; do not
present it as the final architecture. Cleanup exits must supersede the pending
exit, while cleanup-local handled exits must not.

Required regressions (interpreter, plus mixed bytecode/closure paths):

- `(catch 'outer (unwind-protect (throw 'outer 1)
    (catch 'inner (throw 'inner 2))))` returns 1.
- Pending signal survives a cleanup-local handled signal, preserving original
  condition and data rather than the cleanup signal.
- Allocated outer tag/value remain identical after a cleanup-local exit and
  repeated collections; nested cleanup throw overrides the pending throw.
- Reuse one Env after each success/failure, with original catch/binding/frame
  checkpoints unchanged. Test missing/retired payload fallback without panic.

## Shared condition normalization and matching

Introduce one normalization boundary in eval.rs, used by interpreter and VM.
Normalize Signal and same-owner LispError to rooted `(SYMBOL . DATA)` values;
normalize unstructured host errors to the existing generic ERROR diagnostic.
Throw stays a separate nonlocal-exit kind and is never normalized to a condition
merely because a condition handler is installed. Normalize once per pending exit
and retain its root across matching and cleanup; do not destructively downcast
an anyhow error before knowing whether a handler matches.

The shared matcher should accept a condition symbol or a proper list of condition
symbols and return match/no-match without consuming the exit. It must consult
symbol identity and the environment's `error-conditions` property, not compare
names or rendered diagnostics. Bootstrap canonical builtin condition properties
and expose ordinary Lisp property changes consistently. Verify GNU behavior for
unknown signal symbols, missing versus explicitly nil properties, T/DEBUG,
malformed condition lists, and inheritance before freezing this API. Avoid a
hard-coded string taxonomy that would conflict with Lisp-defined conditions.
Use an explicit bounded/cycle-safe property-list scan: mutable Lisp lists must
not turn handler selection into an unbounded recursive walk.

Only a selected handler restores its saved frame/binding/operand checkpoint and
receives the normalized condition. Nonmatching handlers are retired while the
original exit continues to the enclosing frame. Interpreter and VM must share
these rules, including condition-case handler ordering and exceptions thrown by
handler bodies. Keep catch registration restoration separate from condition
matching and retain the established lazy decoder-error precedence.

Then add structured constructors at error origins: no-catch, wrong-number-of-
arguments, wrong-type-argument, void-variable, etc. TypeError's current owned
rendering cannot supply the original object: migrate its producer sites to a
rooted/context-aware constructor instead of parsing the diagnostic. Keep this
migration separate from matching so incomplete producers remain explicit.

## Resource hardening stages

1. Make ordinary frame entry and argument finalization fallible, validating
   counts before publishing frames or changing argument metadata. Propagate
   through call, funcall, apply, interpreter calls, and tests. Reject overflow
   without truncation or unwrap; retain exact ordinary arity errors in range.
2. Make decoder/cache construction fallible with checked/fallible reservations.
   Preserve lazy invalid-opcode and truncation entries: allocation failure may
   fail entering code, but an unreachable bad opcode must not become eagerly
   fatal. Retain owned, GC-independent metadata and content-based cache keys.
3. Add an explicit embedding policy for maximum code bytes, decoded-cache bytes,
   operand storage, handler/frame count, instructions, and host recursion. Do not
   invent arbitrary incompatible caps inside opcode semantics. Share execution
   budget across nested interpreted/native/bytecode entry where appropriate so
   alternating call paths cannot reset it. Account for backward branches and
   polling/cancellation without acquiring unrelated heap locks.
4. Define the condition raised on budget exhaustion and whether it is catchable.
   If catchable, repeated handlers must not replenish an exhausted budget. Keep
   enough reserved capacity for cleanup/error propagation, and test that
   dynamic bindings and catch/frame state still unwind.

Validation gates: source review, isolated focused tests, complete bytecode and
interpreter suites, then GNU batch differential fixtures where available.
Characterize behavior for cyclic/malformed lists and boundary counts without
requiring huge allocations. This plan does not establish protection from native
infinite loops, arbitrary host OOM, or unsafe extension code.


## Prototype validation snapshot

The incremental pending-exit patch replaces Env's single pair/generation with
one HeapDomainValue-backed pair per ErrorType::Throw or Signal. The owner is
validated on binding. Foreign/retired throws fail binding rather than recreate
identity; signals use a fresh generic ERROR diagnostic fallback. Construction
does not render arbitrary tag/data graphs, including cycles.

On published eac483c in a dedicated target: all six pending-exit tests
passed, complete bytecode tests passed 57/57, and interpreter tests passed 19/19.
The pending-exit group includes live foreign-owner binding, retired-owner
cross-thread fallback, and cleanup-local exits. These are scoped
functional regressions, not a provenance proof for all IntoObject callers.

## Bounded exit diagnostics follow-up

General Cons, vector, and hash-table printers already share a visited set;
direct cycles through those paths are not the concrete missing case. ByteFn's
printer starts independent constant-vector formatting, so a constants vector
that refers back to its ByteFn can recurse. Deep acyclic values and large
argument lists can also produce unbounded trace work/output.

The follow-up patch gives EvalError::add_trace/with_trace a nonrecursive argument
summary: at most 16 arguments, 64 source characters per string/symbol/function
name, escaped control characters, primitive integers preserved, and all compound
or potentially large numeric payloads represented by scalar type labels. It
never traverses container edges or acquires Lisp container locks. Unmatched
throw diagnostics use the same safe tag summary. The copied rooted argument
value is scoped to this synchronous no-GC/no-callback formatting operation.

This changes human backtrace detail, not Lisp condition payloads or tag/value
identity. It does not change the general Lisp printer, bound the number of
backtrace frames, make error construction elsewhere cycle-safe, or sanitize an
arbitrary host error's custom Display implementation. Other producer sites
(e.g. TypeError's owned rendering) still require their own audit.

Validation of the bounded-diagnostic follow-up in the dedicated VM target:
three new regressions pass (self-referential ByteFn constants in native/host
traces and unmatched tags; bounded Unicode/control strings and argument counts;
interpreter cyclic throw/signal identity). Complete bytecode tests pass 57/57,
interpreter tests 20/20, and eval tests 6/6. This is atop the independently owned
pending-exit prototype; its frozen patch remains unchanged.


## Checked counted-call follow-up

Ordinary argument finalization now returns a checked error before changing its
u16 metadata. Counted frame entry validates count and caller-local operand
bounds and reserves frame storage before publishing the frame. Native function
entry propagates finalization errors; optimized bytecode native calls use the
checked counted-entry API. Existing test fixtures explicitly unwrap only counts
they construct as valid.

Apply and funcall stage arguments in a separately rooted vector before creating
the target frame. Apply's improper tail, oversized target count, or staging
reservation failure therefore leaves the original caller stack untouched.
Hook argument forwarding uses the same rooted staging approach. The accepted
boundary is 65,535 values; rejecting a larger call is an implementation guard
for current metadata representation, not a claim of GNU's maximum call size.
This does not make empty-frame allocation or all vector/list allocations
fallible, establish a total-memory budget, fix apply's separate minimum-arity
compatibility gap, or implement a circular-list condition taxonomy.

Checked-call validation: six new count/staging regressions pass, including
65,535 accepted apply arguments, 65,536 rejected direct/apply calls, repeated Env
reuse, malformed tail atomicity, caller-prefix protection, and unchanged
metadata on failed finalization. Full bytecode 57/57, interpreter 20/20, and eval
10/10 pass in the isolated target. No integration tree was modified.


## Fallible decoder storage follow-up

Program construction now checks boundary and instruction-vector reservations,
and the per-VM cache checks map/key reservations before publishing a new entry.
Allocation failures are explicit DecodeError::Allocation values; existing cache
entries remain valid. Metadata retains Vec storage rather than introducing an
infallible shrink-to-box operation. Cache keys remain owned code bytes, and
malformed/truncated instructions remain lazy error entries, not eager failures.

This is not complete OOM recovery: Arc allocation and other runtime producers
still use infallible allocations, diagnostics themselves may allocate, and no
total code/cache budget or allocation-policy cap has been added. Failed cache
reservation/decode does not publish a new entry; invalid cursor targets still
follow the pre-existing decode/cache-then-validate order.
