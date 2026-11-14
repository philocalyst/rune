# Owner overlay-arrow configuration state

GNU Emacs 30.1 xdisp.c initializes overlay-arrow-position to nil,
overlay-arrow-string to the pure C string "=>", and overlay-arrow-variable-list
to a fresh one-element list containing the interned overlay-arrow-position
symbol. The unchanged simple.el later registers next-error-overlay-arrow-position
and its per-symbol overlay-arrow-string property. Rune initializes exactly that
pre-simple configuration in its traced owner Env.

These are writable, special, non-automatically-buffer-local variables. Values and
the registration list retain actual identity, aliases, dynamic binding and GC
behavior; arbitrary values are not prevalidated into a fictitious renderer state.
Builtin source aliasing is rejected while other aliases can target the cells.

The initial string is a fixed immutable global leaf, allocated only while holding
the existing global symbol/block mutex. It contains no owner Lisp references.
GNU no-X child probes crashed when attempting to mutate its pure C string;
Rune deliberately rejects those mutations instead of emulating a crash. The
variable can be rebound, and a copied owner-local replacement string is mutable.

This supplies genuine configuration cells, not a Lisp Marker implementation or
arrow rendering. The tests retain arbitrary cyclic raw state, not fake markers.
No fringe/margin glyph publication, bitmap support, marker-row selection,
redisplay cache, graphical capability, whole simple.el load or Org/Doom workflow
is established by the state seed alone. Future publication must resolve symbols
and buffer-local values on the owner, validate marker owner/buffer/liveness and
copy only bounded plain glyph data to the renderer.
