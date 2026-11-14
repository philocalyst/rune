# Single-line strike-through

The owner resolver publishes nil/t/color-string strike-through values from
selected-frame defaults, named inherited faces and anonymous buffer face specs.
Unspecified inherits; nil explicitly disables an inherited line. Foreground-
following t resolves only after final foreground/background inversion. Explicit
colors stay explicit. Nonboolean/non-color or structured styles fail publication
with static errors and remain repairable by later Lisp edits.

Face protocol version 3 adds only a plain optional strike-through color. Lisp
objects remain on the owner thread. Native and SVG PaintTargets receive the same
one-logical-pixel line, vertically capped to row height and placed at baseline
minus half the fixed-grid ascent. Its rectangle is clipped to glyph cluster,
row, viewport and frame, with finite metrics checks. This placement is a declared
fixed-grid approximation, not GNU's font-specific strike position/thickness.

No layout, hit-test, selection or IME authority changes. Exact plain style
comparison detects direct mutable face/plist changes without relying on setter
epochs. Fonts/weights/slants, overlays, complete Org styling and OS-window input
or paint remain unverified or unsupported beyond earlier documented slices.
