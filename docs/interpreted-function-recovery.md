# Opaque interpreted-function foundation

Reconstructed after cloud reset, based on published 3699a33. Earlier unpushed
artifacts and runtime evidence are missing. This is not byte-identical recovery
and has now been checked and exercised in this environment.

Distinct immutable tag stores a private ObjCell pointing to a traced field vector (3/5/6 GNU slots).
No public constructor/predicate, Function admission, evaluator producer, cconv
hook ABI or bytecompiler source changes. Existing Cons closures remain legacy.
Thus no actual source gate is unblocked and no callable functionality is claimed.

Shared Context excludes GC while field views are live. Caller roots returned
objects before mutable Context/callbacks. Pure-GC fields have no native owning
payload or finalizer ledger. Sameowner internal constructor is explicitly unsafe.
Trace/move updates vector then all fields, including captured Cons cycles.

Read-only aref/length do not widen array/vector/sequence predicates. Bounded
contextual equality compares fields; distinct cyclic graphs fail closed (GNU
supports equal cyclic graphs, so this is an explicit limitation). Rust equality
remains identity; public structural hash keys reject the new type. CloneIn,
global function publication, dump (before file write), and serializer reject it.
Current published hash model differs from previously unpushed strategy work;
collector/hash rebases require a fresh exhaustive tag/edge audit.

Before integration with a weak collector, add and review the relevant heap-state
arms. This foundation does not by itself activate interpreted functions.
Later call/introspection migration must preserve shared captured-cell identity.
Producer migration is separate: bundled cconv hook has two arguments versus
GNU30 five; bundled bytecomp destructures Cons closures. Never classify Cons
shape or arbitrary hook results as genuine opaque interpreted provenance.

## Recursive trace correction

The initial reconstruction used Slot<LispVec> inside the heap payload. Root Slot
tracing immediately drains the trace worklist, so deep function-to-environment
chains can recurse through nested drains. Corrected to private ObjCell: its Trace
moves the vector and queues its trace; vector elements already queue through
ObjCell. No new recursive drain, collector edit or native-resource payload.
Added a 20,000-node function chain with three forced GCs and a 600-node distinct
comparison requiring bounded equality error. These tests pass in this environment; they do not establish stack safety
for all collector paths.

Source coverage now also includes global clone_guard rejection, public Lisp
type-of/cl-type-of and vectorp, aset failure without field mutation, and dump
Unsupported before overwriting an existing image. Current published source has
no native arrayp/sequencep primitive, so no such public-predicate success is
claimed. The opaque tag is not added to any array/vector/sequence acceptance
arm; LENGTH/AREF are deliberate exceptions. Typed aset condition parity remains
a preexisting generic TypeError limitation, not a claimed structured arrayp fix.
The reconstructed tests have been compiled and exercised after the reset.
