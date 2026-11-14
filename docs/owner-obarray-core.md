# Owner-local Obarray foundation

LispObarray is a distinct GC-managed object, not a vector, generic hash-table or
native global side map. Its private membership map owns independent native UTF8
name keys and ordinary traced symbol cells. Interning allocates real moving
owner-arena symbols; it never sets the global-interned flag that would skip their
relocation. Names are copied separately into symbol storage. Table membership
roots symbols, not vice versa; uninterning does not destroy separately retained
symbol identity, value or function state. Local names `nil`, `t`, and the empty
name are ordinary local symbols, not builtin constant identities.

Every operation authenticates the private owner arena and liveness. Shared
Context access excludes evaluation/callbacks/moving collection while native map
borrows and returned raw symbol snapshots are used. A private, narrowly audited
lifetime bind returns owner symbols independently of a temporary RefCell guard;
no arbitrary payloads can be inserted through this API. Before callbacks, callers
must root the COMPLETE snapshot vector and release all native map borrows. The
collector traces each member cell and moves symbol names normally.

The native map has one owner in the GC header. Forwarding transfers it once;
dead-header and unpublished-copy cleanup uses existing deferred native finalizer
and rollback tracking. Same-owner cloning returns the actual object identity.
Global, transit and foreign arena copies reject explicitly rather than creating
an inert shell or widening owner pointers. Dump output rejects before file write;
there is no readable restoration design in this foundation.

Capacity and active entries are capped at 65536, each name at 1 MiB, aggregate
active name bytes at 16 MiB. Checks precede copies/reservation and publication.
These are operation/state bounds, not total heap/OOM or CPU-preemption promises.
Enumeration order is unspecified. Raw non-UTF8 symbol names, legacy vector
obarrays, a genuine dynamically-bound default `obarray` variable/global namespace
proxy and full mutation/enumeration parity are not established here. Existing
omitted/NIL namespace fallback is not a claim to implement that default variable.
No fake empty local table substitutes for the global symbol namespace.
