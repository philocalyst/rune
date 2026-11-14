# Process ownership boundary

A genuine process tagged object holds only a shared native transport, immutable
name, nonreused native identity and owner domain. OS PID is metadata, never the
registry identity. It contains no Lisp callbacks, buffers or Context reference.
Moving GC stages owning process wrappers with other native finalizers and drops
them after collector guards. Foreign-domain clones share only native state;
operations requiring the owning Context reject before callback/root access.

The owning Context retains process/filter/sentinel/buffer/plist in existing
HeapDomainValue roots. Snapshot clones own those roots without keeping a registry
borrow across evaluation; snapshots are owner-thread-only. Setters publish fresh
roots before replacing old ones. Raw Object entry points are unsafe with explicit
same-owner/global reachable-graph and temporary-root preconditions, not arbitrary
payload provenance validation. Native coding/query/event state has no GC values.

Context teardown requests termination before retiring roots and collecting the
heap. It never joins workers or waits for child reaping; the native supervisor
owns that duty. delete-process must terminate without unregistering callbacks;
completed records are excluded from active lookup/polling after sentinel delivery,
but retain metadata until Context teardown so retained process handles keep their
filter/sentinel/buffer/plist. This strong-record retention is an explicit resource
gap, not weak/ephemeron lifecycle completion. Explicit remove_process is an owner
retirement/testing API, not the ordinary sentinel path.

This requires the cancellation-aware and natural-exit transport supplements.
No process-group/descendant pipe cleanup, complete Emacs process compatibility,
PTY support, native GUI event-loop pumping or arbitrary panic/OOM recovery is
claimed. Heap dumps map ephemeral process capabilities to nil, never serialize
native pointers or reconnect a child process.
