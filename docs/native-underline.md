# Single-line underline publication

The immutable face DTO (display protocol version2) carries a resolved optional
underline color. Owner resolution supports GNU underline nil (explicit off), t
(final merged foreground), color strings, unspecified inheritance, and the
existing bounded named/list/anonymous buffer face reference subset. The raw
Foreground marker is retained until foreground inheritance/overrides finish;
explicit Off remains distinct from Unspecified. Default-face underline t also
uses a named run's final foreground rather than its earlier default color.

The shared painter emits bounded single-line logical-pixel rectangles under
underlined clusters. Native GPUI receives those through its existing fill path;
SVG uses the same geometry. Rectangles are clipped to cluster, row, viewport and
frame, capped for subpixel rows, and rejected for nonfinite metrics/baselines.
The thickness is one logical pixel, at baseline + descent/4 within the row. This
is an explicit fixed-grid decoration contract, not GNU font-selected thickness
or full redisplay parity. No second GPUI text-decoration flag is enabled.

Layout, text advances, hit testing, UTF16 context and input sequence authority do
not change. Mutable underline colors/flags participate in existing exact plain
style DTO revisions. Structured wave/double-line underlines, font weights/slants,
heights, overlines/strike-through, overlays, full Org/font-lock rendering, and OS
window validation remain outside this slice. Tests use genuine owner metadata,
FIFO publication, and deterministic headless paint, not a claimed GUI screenshot.
