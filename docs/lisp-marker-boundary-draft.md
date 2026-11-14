# Lisp marker boundary (draft, not release-approved)

A LispMarker is a genuine owner-arena GC object, not an integer or a position
cache. Its header owns one stable, reserved Session marker identity and native
retirement duty for its entire logical lifetime, including detach/reattach and
cross-buffer transfers. Its optional attachment carries authenticated target
and owner/buffer liveness witnesses, not a second retirement duty. The sole
position truth remains the editor registry. GC movement transfers the duty; finalization
only marks a preregistered native retirement cell, never borrows the editor.

Ordinary attached markers do not root themselves or retain text/history leases.
The buffer's canonical `mark-marker` has a separate origin-buffer root, including
while detached. Killing origin A releases that root but must not detach an
externally retained marker moved to live target B. Target death makes marker
queries detached. Foreign/global graph copies reject rather than inventing a
second active registration.

Primitives preserve GNU ordering and identity: explicit BUFFER type is checked
before MARKER; a known dead buffer then detaches without parsing POSITION;
otherwise integer/marker positions clamp in the target. Default target is current
buffer even when POSITION is a marker elsewhere. `copy-marker` instead follows
the source marker's buffer, creates a distinct object, and defaults insertion
type to nil. Insertion-type setter returns the caller's original value, while
its getter reports a boolean. Native source/target document updates are one
prepared atomic commit. Native history retains the same logical ID and applies
deletion offsets only when current position and insertion bias remain eligible:
resetting to the collapsed anchor, detach/reattach, and a cross-buffer roundtrip
restore GNU offsets; moving away or changing bias does not blindly restore old
positions. Earlier independent edits are not rolled back.

## Comparator audit

- `eq`/`eql` retain identity for markers.
- Lisp `equal`, `assoc` (default comparator), `rassoc`, `member`, `delete`, and
  bytecode Equal/Member must use the bounded Context-aware comparator. Marker
  leaves query the authoritative owner; detached markers compare equal and
  attached markers compare target buffer/position, not insertion bias.
- Cons/vector/record/bytecode constant children are explicit pair tasks; work,
  depth and pending edges are bounded. Distinct cyclic graphs reject instead of
  recursively exhausting the process. Opaque hash/char tables compare native
  identity and do not recursively compare their stored children.
- Existing Rust hash keys have no Context-aware comparator and make-hash-table
  does not yet preserve a test strategy. Lisp hash operations therefore reject
  marker-containing structural keys before lookup/mutation for every test. They
  never silently use marker identity as positional `equal`. Marker values are
  allowed. Internal Rust-only identity/structural helpers are not GNU `equal`.
- Property-bearing `equal-including-properties` remains explicitly unsupported,
  rather than discarding property spans during comparison.

This does not implement arbitrary marker-aware arithmetic/argument conversion,
narrowing, mark activation or complete buffer-local-variable behavior, keyboard
commands, readable marker serialization, or full Org/mu4e/Doom success. Generic
Rust PartialEq on the native header is identity only and is not a GNU semantic
claim. All paths still require isolated combined tests and independent GC review.
