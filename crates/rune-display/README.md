# First-light display foundation (M1 / DE-01..06, I3/I4 draft)

This is a new **pure, unsafe-free display engine**, not another set of Lisp
primitive stubs. Its input is an immutable, thread-safe text snapshot plus
prepared face spans and explicit per-window geometry. Its output is a versioned
immutable frame: source-mapped clusters/rows, selection rectangles, cursor and
hit map. Painting consumes that exact geometry through `PaintTarget`.

## Run without a GPU/toolchain dependency download

```
cargo test --manifest-path crates/rune-display/Cargo.toml --features rope
cargo run --manifest-path crates/rune-display/Cargo.toml --example first_light -- /tmp/first-light.svg
```

This crate is temporarily its own workspace, so these commands never acquire the
main Rune workspace build lock. The same is true of `rune-gpui`. Root workspace
membership should be migrated together with the text/editor crates, not by
introducing optional GUI dependencies into the existing interpreter binary.

`../rune-gpui` implements the native GPUI 0.2.2 canvas paint adapter. It is source-
backed but **not yet built or window-tested in this environment**. `rune-gpui/examples/first_light.rs` now supplies application/window setup and
keyboard event reporting, but remains **uncompiled and not window-tested**: the
offline dependency cache has no GPUI package. Native font metrics calibration,
IME, resize/scroll events and live editor-to-UI repaint delivery remain host
integration work, not claimed done. The headless SVG path and core layout
are executable now. A complete M1 GPU window/100 MB benchmark gate is NOT claimed.

## Boundary with text/editor lanes

* Positions are **zero-based Unicode scalar indices**, not UTF-8/UTF-16 bytes or
  Emacs's one-based buffer positions. The runtime boundary adds/subtracts one.
* Feature `rope` provides `RopeSnapshot`, a cheap newtype around
  `rune_text::Snapshot`. Iteration seeks the anchor directly;
  `next_line_start` uses the rope newline index. The manifest currently references
  the sibling text-overhaul lane; change that dependency to `../rune-text` when
  both lanes are merged into one workspace.
  No live buffer mutex or GC pointer may enter this crate. `StringSnapshot` is
  an indexed immutable fixture: initial indexing is O(n), clones are O(1).
* The editor supplies stable `u64` window/buffer IDs, point, selection, anchor,
  wrap/truncation, logical-pixel viewport and offsets. `vscroll` is the fractional
  first-row displacement. Anchor selection/Emacs scroll policy remains prepare's
  responsibility; a mid-logical-line anchor resets logical tab columns.
* Face spans must be sorted, disjoint and pre-resolved. No eval/fontification in
  layout. Baselines, caret and mouse hit positions all use the same row geometry.
* UI clicks carry buffer revision. The editor must translate or reject stale
  positions. Committed inputs use bounded `InputSender`; full/disconnected queues
  return the undelivered event. Quit's atomic flag is set before queue delivery.

## Bounded work and publication

Layout stops at visible rows plus at most 64 overscan rows, 4096 rows total, and an
explicit scanned-character budget. Truncated long lines jump via the newline
index rather than scan hidden tails. Wrapped long lines resume through
`next_anchor` if the character budget is exhausted; this is observable, not an
infinite-loop fallback. The fixture's million-character test exercises this path.
`LayoutCache` shares unchanged row Arcs across cursor-only frames and adjacent
text revisions, with bounded window retention. It currently recomputes visible
rows to compare them, not a full incremental summed layout tree.

`DisplayMailbox` is latest-only and rejects out-of-order generations. Its mutex
is held only for swapping/cloning an Arc, never layout or painting. It is not a
lock-free ArcSwap and does not itself schedule a UI repaint. Input is nonblocking.

## Deliberate limits

The deterministic metrics fixture assigns one cell per ordinary scalar. A real
metrics backend supplies wide/zero-width cell advances. This is **not** bidi,
ligature/combining-cluster shaping, GNU glyphless/display-table semantics or font
fallback parity. Native adapter currently shapes isolated display clusters;
full-run shape caching will replace it. Tabs and ASCII controls are expanded;
wrapping keeps expansion clusters atomic. Images, overlays, replacement specs,
mode/header/minibuffer lines, IME, split-window policy, Lisp redisplay scheduling
and automatic scrolling are later layers. No terminal frontend is introduced.

## Verified in this environment

`cargo test --features rope --offline`: 22 tests passed (21 integration tests and
one rope adapter unit test). The SVG example generated 30 rows while scanning
1,220 scalars. The 100 MB test is a **lazy synthetic snapshot bounded-work test**,
not an allocated-rope memory or GPU benchmark. The GPUI example check stopped at
dependency resolution (`no matching package named gpui`); no native compilation
or window launch is claimed.
