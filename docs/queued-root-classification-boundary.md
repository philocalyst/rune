# Real queued-root classification boundary

This test-only increment is based on published tree
ed16898309965d07e01bbdc8321b2dad816e5f5b. It changes no collector, queue policy,
root registration, transfer guard or public operation. It does not implement
queued-cycle reclamation. The earlier native-ID model is not its evidence.

## Two different real runtime roots have the same external witnesses

The first test sends a real receiver through its own channel using production
send/copy logic, closes both external endpoints, and forces owner and manager
moving collection. No external residence remains. The queued receiver's family
still keeps receiver_count=1 and its single queue entry remains after three
collections. This records a current retention limitation, not desired cleanup.
It makes exactly one capacity-one cycle. A private administrative dequeue then
closes the recovered endpoint and collects; the test asserts channel removal
and empty residence registry. No unbounded stress leak is created.

The second test uses the actual production copy_for_queue preparation and
receive_prepared transfer path. Its HeapRoot has not entered a queue. It has
zero external residences too, yet it MUST survive collection: a native send or
receive operation owns that transfer. Three moving collections preserve it,
then normal receive-copy recovers a live externally registered receiver.
Explicit close plus collection removes the channel. This is real moving-root
execution only after the corresponding recorded successful test run; it is
not a concurrent blocked-send or dequeue/publication race test.

## Precise blocker to production reclamation

ThreadSafeRootSet currently stores only opaque `*const dyn Trace` entries;
queued and in-flight `__HeapRoot<Slot<Object>>` values register identically.
collect_garbage_raw traces every registered root before closure/finalization.
There is no sound way to skip roots based ONLY on an empty external-residence
snapshot: it cannot distinguish the two tests above. Skipping all such roots
would free a live transfer and leave its rooted slot pointing into from-space.

A real next implementation must distinguish queued roots (authenticated
manager identity plus channel membership) from unconditional prepared/dequeued
flights. Registration and each transition require overlap, never a root gap.
Queued-to-flight promotion must commit BEFORE the message becomes inaccessible
to a queue snapshot; flight-to-queue publication must be visible BEFORE strong
flight membership disappears. Mutable native tags without atomic publication
and collector exclusion are not a proof of that handoff.

The existing collector takes gc_access WRITE -> gc_state -> roots mutex.
Copy preparation takes gc_access READ -> gc_state -> roots mutex. Channel data
locks currently do NOT exclude collection, because every queued root remains
unconditional. A collector which also snapshots/prunes channel data introduces
a new lock edge. Do NOT add data-lock -> gc_access READ: with a waiting writer
that needs the same data lock, writer preference can deadlock. Acquire access
before data for nonblocking transitions, release it before condition-variable
wait, and revalidate capacity/closure after reacquisition. Last root/family
callbacks must remain deferred beyond all collector/data/registry guards.

An epoch's external/operation witness set must be monotone and pin residences
until after collector guards; newly admitted native/foreign-manager endpoints
must not be omitted. Reached transit endpoints must enroll their channel's
queues into the SAME graph closure before pruning/finalizers/from-space drop.
Any future weak-table ephemeron closure must join this loop, not run before it.
Budget/error handling must fail before publication, not silently free unknown
roots or substitute a purportedly precise strong fallback. Current published
collector remains unconditional; no weak-table support or cycle parity is
asserted by these tests.

## Candidate exclusion protocol to review next (NOT implemented)

An alternative to changing every queue transition is to freeze ALL existing
channel data under the manager's collection exclusion: gc_access WRITE ->
gc_state -> channel-map READ -> each channel-data mutex -> root-set mutex.
Current sends prepare outside data locks; receives release data before taking
gc_access. This potentially permits a stable queue-root identity set while
prepared/dequeued roots remain in the unconditional registry. It requires a
complete fresh lock audit; the ordering above is a proposal, not a proven API.
Holding every queue mutex through a collection can block unrelated channels
and is not a latency guarantee.

A plan would keep authenticated native channel indices and per-queue root
identities, seed channels from pinned external residences, trace non-queued
flight roots unconditionally, and let reached same-manager endpoint leaves
schedule their queue roots. A worklist would repeatedly drain both ordinary
object tracing and newly enrolled queues before any weak/finalizer sweep.
New collection context must not make default Context tracing accidentally
conditional or confuse a foreign manager's same numeric channel ID.

Dead queue root REGISTRATION must disappear before its from-space values are
freed. Merely moving still-registered HeapRoots to a deferred Vec is unsafe:
if exclusion is released before that Vec drops, another collector can see a
registered pointer into old space. A typed retire-registration operation (or
otherwise audited immediate deregistration without native callbacks) must
precede commit, while owning native endpoint destruction stays deferred until
all locks are released. Surviving queue slots must already be forwarded.
The old HeapRoot API currently couples registration removal and Box teardown.

Finally, current Trace is infallible and mutates forwarding headers. A bounded
or fallible preflight must reject BEFORE any irreversible movement, or support
an audited rollback/retained-from-space strategy. Discovering a work-budget
error after partial movement and continuing with dropped unknown roots is not
fail-closed. The exact production plan, root identity/retirement API, data-lock
freezing protocol, endpoint enrollment and concurrent flight tests are still
missing; this increment must not be read as permission to activate reclamation.
