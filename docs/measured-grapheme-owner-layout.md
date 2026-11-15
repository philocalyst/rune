# Experimental measured grapheme owner layout

This independent, explicit API is not called by EditorBridge, FrameView or native
startup. It establishes a small owner-layout consumer contract, not a contextual
measurement producer, shaped text, painting, or styled Org support.

`layout_measured_graphemes` consumes an immutable TextSnapshot, caller-current
window/buffer/owner-sequence/publication scope and bank/geometry descriptor, plus
a plain measured-cluster plan. It checks exact scope equality, document revision,
complete source text and capabilities. These comparisons are correlation, not a
security or provenance proof: the caller must obtain current scope from its owner,
and trust the producer asserting contextual widths. A nonzero producer ID records
that assertion only. DiagnosticScalarAdvanceSum and Unavailable are rejected.
No current native diagnostic is promoted to the trusted producer role.

The first slice requires a complete single logical line of at most64scalars and
256UTF-8bytes, with at most64clusters/exact boundaries. Unicode segmentation1.12
validates exact whole-text grapheme coverage. Widths must be finite, positive and
fit the viewport. Controls (including newline/tab), scrolling, partial viewport,
non-wrapping mode and all face spans reject. A caller with unresolved style or
font-lock properties must not choose this uniform-default-face API. Anchor must
be0; point, selection endpoints and supplied extra input/IME boundaries must be
exact grapheme boundaries. Nothing is snapped or rounded.

Greedy wrapping treats each cluster atomically, preserves every supplied width
and exact scalar source range, and derives rows, selection rectangles, caret and
existing hit-test geometry together. It is not GPUI word-wrap parity. Oversized
clusters, insufficient viewport height, malformed coverage and arithmetic overflow
fail before any output is published. Positive row/cluster/baseline advances must
remain representable at their coordinate origins; unavailable visible caret
geometry also rejects rather than silently dropping the cursor. Zero-width standalone clusters are outside
this first contract, rather than silently losing hit-test progress.

Four pure tests cover fixture-width geometry/hits, protected interior boundaries,
styles, stale scope/bank/source, unsupported provenance and invalid widths. One
actual Session<RopeDocument> fixture uses its owned publication snapshot, proves
layout, then rejects direct-point away/back and text-edit stale plans without
changing owner input. Fixture widths are explicit test numbers, NOT measurements
of native shaping. The normal scalar layout remains unchanged. Genuine contextual
width production and owner/UI lifecycle integration are still required before
activating this path in a native editor.
