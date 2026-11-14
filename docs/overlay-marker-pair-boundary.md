# Overlay marker-pair owner boundary

Pair registration, repositioning, cross-buffer transfer and removal derive all
immutable document roots before publishing any root. Both scalar positions and
existing marker membership are checked; source and target buffers are explicit.
IDs remain globally nonreused and a failed derivation consumes allocated IDs.
Independent start/end biases are preserved. Generic marker pairs do not normalize
endpoint order; overlay primitives own that policy.

Marker-only operations do not change text, properties, dirty state, point,
selection, publication epoch, input sequence or text undo history. Current live
marker membership remains independent of historical document roots. Transfer
preserves pair IDs and removes them from the old live buffer only after the
new target root is ready. Deleted/detached overlays register new IDs on reattach.

`BufferLiveness` is an opaque cloneable Send+Sync native witness, with no public
setter, editor borrow, document root or Lisp value. It closes after a successful
buffer-kill transaction commits, never for a failed/rolled-back kill, and when
the Session drops. Same-name replacement buffers have unrelated IDs/tokens.
It is NOT Context/domain authorization: GC attachment ownership must additionally
retain Context's separate owner-liveness witness, since escaped gateway clones
can keep a Session allocated after Context retirement. No rendering/properties,
Lisp overlay API, full GNU overlay fidelity or mutation-hook behavior is claimed
by these native owner primitives alone.
