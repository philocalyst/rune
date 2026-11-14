# Backward buffer regexp search

The primitive searches the owner current buffer, using one-based Unicode scalar
positions. It chooses the nearest starting position whose match ends no later
than point. Captures, partial-count failure, point restoration, NOERROR's
non-t move-to-bound behavior, count zero, and negative (forward) counts are
qualified against GNU. Matching sees the full buffer, not an artificial substring:
anchors and lookarounds do not treat the search ceiling as end of buffer.

Scope remains the existing Lisp-to-Rust regexp translation and fancy-regex
engine, not full Emacs regexp syntax/case tables. Case-fold-search is scoped
through Env, not a new per-buffer case-table implementation. Buffer text must
fit 4 MiB, absolute repeat count is at most 10,000, and backward candidate probes
at most 100,000. Engine backtracking limits also apply. These are explicit
resource errors, not GNU behavior or a hard wall-clock/peak-memory guarantee.

Existing Env match storage uses integer positions, not live marker pairs. This
primitive updates match-beginning/match-end in buffer coordinates, but does not
add narrowing, marker bounds, automatic match-data adjustment on later edits,
full match-data marker/REUSE/RESEAT behavior, or property-aware text matching.
