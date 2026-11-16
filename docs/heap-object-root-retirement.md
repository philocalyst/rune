# Opaque object-root retirement prerequisite

This isolated increment adds a consuming retirement operation for the existing
`__HeapRoot<Slot<Object<'static>>>`. No collector calls it in production yet.
Queued roots remain unconditional. It does not classify flights/queues,
activate cycle collection, free queues, or implement budget failure recovery.

## Ownership and destructor ordering

HeapRoot now stores a private registration guard FIRST and its existing boxed
Rt<T> SECOND. Rust drops fields in declaration order. The guard removes root
membership and releases the root-set mutex before the boxed T destructor runs,
just like the previous explicit HeapRoot::drop implementation. Construction
still registers the same pinned Box allocation; moving either Box or HeapRoot
does not move the pointed-to slot. No new unsafe block or lifetime conversion
is introduced by this refactor, and normal roots retain existing behavior.

The specialized consuming retire method destructures that root, explicitly
drops the registration guard while Box storage still exists, then returns only
opaque RetiredObjectRoot storage. The latter exposes no Deref, Trace, Debug,
Display, Object conversion, or value accessor. It is deliberately !Send/!Sync
via a private Rc marker and must remain on the collector's native stack. There
is no registration guard left to remove twice or to re-register accidentally.

This API is NOT generic over Trace. Arbitrary Rust payload destructors might
read GC references. The exact Object, Slot<Object> and Rt<Slot<Object>> payloads
have no destructor, verified by needs_drop tests. They contain nonowning tagged
pointer bits, not Rust ownership of Lisp/native payloads. Retirement may leave
those pointer bits stale after old-space release. Dropping retired storage only
tears down the still-valid native Box allocation; it never reads, prints,
traces or dereferences the stale Lisp pointer. Changing that payload's Drop or
exposing its data requires a fresh audit and must fail the destructor test.

Consuming a root gives up exactly its own liveness protection, just like normal
root Drop. Other registered aliases remain independent and continue forwarding.
This does not revoke previously copied raw Object aliases or exempt their
callers from heap-domain/collection-exclusion contracts. Safe reads from retired
storage are impossible through this API; prior unsafe get_inner uses must still
not escape their synchronized scope. A future queue collector must separately
prove no live transfer was retired and protect each membership handoff.

## Intended future collector ordering, not activated

Remove an unreachable queue root from its queue, consume its registration BEFORE
from-space release, and retain only opaque storage until collector/data/registry
guards have been released. Owning endpoint payload finalization remains in the
existing DeferredEndpointFinalizers batch, also outside those guards. Never
defer a still-registered dead root beyond collection exclusion: another
collector could otherwise trace the freed from-space pointer.

The method does not recover a poisoned root-set mutex or a mid-collector panic.
Those are the same unsupported conditions as the previous root Drop path.
Native Box allocator behavior, OOM, arbitrary Rust panics and total retained
memory are not contained. Holding retired storage forever deliberately retains
only its small native slot allocation, not the referenced Lisp graph.

## Qualification boundary

Native tests verify ordinary generic payload destructor ordering, outer native
scope unwind ordering (not mid-GC recovery), and absence of Object-slot Drop.
Real manager tests retire under gc_access/gc_state/channel-data guards, perform
raw moving collection while retired storage survives, and release deferred
endpoint callbacks only afterward. Another root of the same object survives
three collections, is received normally, then closes and collects; dropping the
retired storage does not restore root/channel membership.
Both cases explicitly remove their bounded channel and residence membership.
These are successful execution claims only with the matching recorded tests;
no end-to-end conditional root or concurrent transfer proof is implied.
