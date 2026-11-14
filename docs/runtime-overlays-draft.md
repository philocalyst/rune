# Overlay adapter draft — production hold

This adapter is intentionally not published until the real GC Overlay type and
all cross-arena copy rejection paths are proven. It is not a vector stand-in,
an inert transferred shell, or a no-op make-overlay.

The proposed implementation uses owned editor marker pairs, separately stored
front/rear insertion biases, and the Context's canonical attached-object roots.
Properties are traced ObjCell fields so detached property cycles can collect.
Opaque buffer and owner liveness witnesses allow killed-buffer pruning without
borrowing editor state during GC. Delete retains properties; move can reattach a
detached object. Cross-buffer marker transfer must be one atomic owner edit.

Positions use the current Unicode buffer's scalar coordinates. Make/move clamp
and normalize integer endpoints. Omitted move BUFFER retains an attached
object's buffer; a detached object uses current buffer. For an empty
front-advancing/rear-staying pair, logical crossover collapses at the rear rather
than incorrectly including inserted text. Property spines are copied on read;
new property keys prepend. Explicit nil properties shadow category defaults.

Unsupported scope is explicit: marker-object endpoint arguments, narrowing,
priority-sorted queries, evaporation and edit hook callbacks, overlay rendering
and full face/invisibility composition. Direct non-nil evaporation/hook setters are rejected
before property mutation. This is not an effective-hook fail-closed guarantee:
category fallback can supply hooks or evaporation, including later category
mutation; editor callbacks and category-derived behavior are unsupported. Range queries scan attached overlays; this is not an
indexed overlay rendering/performance claim. No text buffer is flattened by
these operations.

Four runtime adapter tests cover: Org's make/put/delete
initialization, Unicode edits and undo/redo, empty bias behavior and copied
properties/category fallback, cross-buffer move and killed-buffer lifetime.
The isolated combined suite passed 802 tests before the final private-getter
hardening; the V2 qualification is recorded separately. No original Org source
load or overlay rendering success is implied.
