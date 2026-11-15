# Opt-in native retained-row staging, not painting

`retained_row_stage` adds a UI-owned staging object. No FrameView, normal
renderer, native CLI or worker calls it. It exposes no paint method, ShapedLine
getter or conversion into owner width provenance.

The `shape` method uses ONLY public WindowTextSystem::shape_line with the exact
bounded request text, one compatible measured normal-font descriptor, the
baseline size and force_width=None. It retains that actual ShapedLine privately,
its request, Rc font bank, Weak TextSystem and originating AnyWindowHandle.
A checked process-local stage ID becomes the reply's UI registry number.
The actual shape invocation has NOT executed: this environment cannot construct
a permitted real Window. Native compilation verifies the public API route.

Preflight validates the actual bank against its normal baseline and App,
request bank ID, supported policy1, nonzero owner window/buffer IDs and complete
source starting0. Policy1 here means the explicit normal single-run descriptor,
not authentic GNU face/style behavior. The request constructor already caps
source64scalars/256bytes and rejects controls. Baseline descriptor carries its
actual GPUI font features; no independent font database is substituted.

After shaping, the method bounds observed runs/glyphs to256, verifies source
identity, finite glyph positions and finite line metrics/nonnegative width.
These are post-shape observation bounds, NOT a bound on backend shaping memory
or CPU. Empty source/zero width is observation data, not automatic layout
admission. All returned observations say coverage Unknown, never verified.
GPUI fallback, opaque glyph identity and scalar metrics cannot resolve coverage.

`matches_receipt` compares the full request key and stage number, shared
revocation state, current Window handle and actual App/Window TextSystem
identity plus live bank identity. Even true is only an observational correlation
check, not owner authentication, atomic liveness, frame publication, coverage,
valid input geometry or paint permission. The Weak TextSystem and Window handle
are not a strong lease keeping the Window alive. No cross-thread native values
or raw Lisp objects are introduced.

## Actual execution evidence

`headless_row_stage_preflight` executes real native font/bank measurement and
preflight only: current bank accepts; a distinct measured bank, partial source
start and unknown shaping policy reject. The final run exited0 with
HEADLESS_ROW_STAGE_PREFLIGHT_PASS, externally bounded by timeout8s (not hit).
It does NOT call the Window-dependent shape or receipt method. No actual native
row/retained glyph runtime, Window, shape, raster, paint or Org styling is proved.

Required before enabling any downstream consumer: genuine same-Window shape
and retained receipt tests; coverage/fallback policy; fresh owner snapshot and
exact accepted frame; synchronized cursor/hit/selection/IME geometry; failure
and wrap behavior; live paint-phase checks and actual paint testing. Current
IME font-plan prohibition remains unchanged.

## SDK failure boundary

GPUI's `shape_line` returns ShapedLine directly, not Result. Font resolution
and backend shaping can panic or abort, including on allocation failure. This
module does not catch those failures. Its Result covers explicit preflight and
post-shape validation only, not every backend failure. Post-shape run/glyph
caps cannot prevent work or allocation already performed inside the backend;
no general resource containment or panic-safe renderer claim is made.
