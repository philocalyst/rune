# Branded owner overlays

An Overlay is a real GC-tagged mutable owner-arena object. Its plist is an
ordinary traced ObjCell; detached self-cycles therefore collect. It is not a
record substitute, native global property table, or independently self-rooted
property lease. The opaque numeric identity is nonreused; exhaustion deliberately
panics rather than wrapping, matching other checked runtime identity allocators.

Only the private Context factory brands its freshly constructed ordinary Block
with the unique Context domain. Global, manager, and unchecked standalone arenas
are unbranded. GC swaps backing bump spaces within the same branded Block.
An Overlay constructor returns only a GC allocation, not an owning portable Rust
header. CloneIn preserves the identical allocation for a matching live arena
brand; global, transit-manager and foreign destinations return Err before sharing
any owner pointer. The native resource ledger does not record identity aliases.
Trusted IntoObject/Gc/raw-move APIs still require their existing provenance
contracts; this is not a whole-heap foreign-Object validator.

Attached overlays are canonically rooted in their owning Context. A root is
prepared before the editor marker-pair transaction, then a callback-free native
attachment commit records the authenticated buffer handle, pair and liveness.
The attachment constructor is unsafe because matching the pair/token/handle is
an audited gateway provenance obligation. Owner stamps and liveness observations
alone do not authorize access or establish marker membership.

Before owner GC, the registry removes attachments whose native BufferLiveness
or OwnerLiveness witness is false. No editor borrow, callback, marker lookup or
text/document lease is required during collection. Owner retirement clears roots
before heap teardown, including when an escaped gateway retains its Session.
Detached or failed preparations explicitly remove the canonical root. Returned
root snapshots remain genuine roots until dropped and reject foreign/retired
owner binding; they are never borrowed Object values across GC.

Dump construction records an unsupported-overlay failure, and into_output
returns Err before any bytes are emitted or an existing image is overwritten.
The internal recursive dump placeholder is never serialized as nil. No Overlay
restoration format or cross-domain adoption semantics are fabricated.

This core supplies identity, properties and attachment lifetime primitives.
The separate Lisp adapters and owner marker-pair gateway define overlay endpoint
and property behavior. Mutation hooks, evaporation, display behavior, arbitrary
foreign graph provenance, general cyclic CloneIn support, recoverable OOM and
mid-GC panic recovery are not claimed by these tests.
