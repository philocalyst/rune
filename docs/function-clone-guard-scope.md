# Function publication clone guard

The existing recursive global function copier does not preserve cyclic graphs.
Before publication, a callback-free iterative walk rejects cycles, depth over
256 explicit container edges, over 100,000 expanded visits/tasks, and over 16 MiB
of estimated copied payload. Repeated DAG paths count again; this is not a
unique-node-only budget. ByteFn constants add an internal vector copy layer not
represented as a separate explicit edge. Header/allocator overhead and all native
execution time are not claimed bounded by the payload estimate.

Validation and copy run under the existing INTERNED_SYMBOLS publication mutex.
Global hash insertion takes that same mutex. Hash and character table snapshots
copy every child under one short source lock, releasing it before traversal.
Concurrent global hash removal may remove edges but cannot add unvalidated ones;
index-by-index scans would not establish that upper bound because shift_remove
can hide a surviving edge. Published conses/vectors/records/character tables are
immutable, and local sources cannot be mutated by another safe owner thread.
The existing source-owner/root provenance contract is still required.

The guard follows actual CloneIn container fields. Symbols are leaves: cloning
an uninterned symbol shares an already-global function pointer rather than
recursively copying its function. Alias-chain cycles therefore need a separate
assignment/resolver guard. Process/buffer/channel wrappers copy native handles,
not their external owner registries or queues.

Valid cyclic or sufficiently large/deep definitions are deliberately unsupported
and fail without replacing the old function. This is not a cycle-preserving
copier, universal stack-size guarantee, whole-runtime timeout, or proof that
other CloneIn boundaries (for example channel payload copies) reject cycles.
