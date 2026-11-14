# Fallible graph copies and unpublished native rollback

CloneIn returns Result throughout recursive cons, vector, record, bytecode,
character-table and hash-table copies. A failed child is not replaced with nil,
a tombstoned shell, or a reported channel-close event. Global function/hash
publication and channel send/receive propagate failure. Channel APIs report the
explicit channel-copy-failed status; a failed receive consumes the dequeued
payload and releases its transit root. Go propagates initial transfer errors
without starting a worker and reports bounded worker/result transfer failures.

The four production arena-copy boundaries use an exclusive unpublished copy
transaction. Newly allocated native-owning wrappers are recorded separately from
normal GC finalizer registrations, including allocations into permanent global
storage. Failure removes provisional registrations and moves ownership into a
batch declared before all symbol-map/collector guards. That batch is destroyed
after the guards release, never while a source container or endpoint registry
lock is held. The uninterned-symbol alias cache is cleared before abandoned
headers can become visible to a later copy. Successful copies commit only after
infallible publication/registration; provisional pointers must never escape.

Rollback covers endpoint leases/residences, processes, frames, native hash and
character tables, BigInt limbs, bytecode opcode boxes, and external String/Vec
storage in the block's drop stack. Abandoned bump bytes remain allocated until
normal arena reset/retirement. Existing successful-allocation finalization gaps
for BigInt/opcode boxes are not fixed by this rollback supplement.

The transaction and allocation-recording APIs are unsafe: callers must exclude
collection/concurrent target allocation, record only distinct new owning
wrappers, avoid nested transactions, and not expose provisional graph values.
Same-arena identity returns must not be recorded as new allocations. Tests use
node-countdown failure injection and explicit unpublished unwinding; these do
not prove arbitrary mid-GC panic recovery or recoverable allocation failure.

This is copy infrastructure, not Overlay integration. Overlay remains absent
until destination-owner branding, same-owner identity, foreign/global rejection,
attached-root lifecycle, explicit dump rejection and every trusted constructor
boundary have been independently reviewed and tested. No global lock is added
around channel copying, and no conditional channel reclamation policy changes.
