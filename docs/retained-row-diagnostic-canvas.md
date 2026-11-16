# Dormant retained-row Canvas prototype

This candidate is based on f0ecf78dd323d70525181025d188f0015e9c0897 (source-index + relative run-font identity guards). No normal renderer or CLI calls the new method.

## Concrete SDK path

`StagedNativeRow::diagnostic_canvas` captures the same private retained ShapedLine and immutable bank in Rc, plus the owner's observational accepted token. It returns the SDK's public Canvas. Canvas::paint invokes its callback during the normal element paint phase; no public phase mutator or private Window constructor is used. The callback rechecks actual Window handle, live token/key/stage ID, same live App/Window TextSystem and bank, and the retained source. It checks finite positive fitting bounds and representable coordinate progress, intersects the enclosing content mask and calls the same retained line's public paint method. It never reshapes into per-character draw calls.

## Evidence

The prior candidate native tests/examples compile check passed. Exact extracted pure geometry tests passed2/2, covering fit/zero source width, oversized rows, zero viewport, NaN, overflow and absorbed coordinate progress. The actual Canvas callback, Window creation, ShapedLine::paint, raster and GPU presentation have NOT executed. Official headless open_window still returns an error; no display restriction workaround was attempted.

## Deliberate limits

This is diagnostic output, not owner geometry or coverage authority. Coverage stays Unknown; same font ID may still contain .notdef or SDK descriptor fallback. Bounds checks do not bound glyph ink or prove shaping correctness. Content masking limits scene visibility, not shaping/raster backend work. Caller supplies normal Styled canvas size and a fresh status Cell initialized to NotInvoked; status is not a trusted receipt. PaintReturnedOk means only the SDK call returned Ok, not that pixels were presented. Paint errors may follow partial scene recording; no rollback. SDK panic/OOM is not caught. Token liveness is observational and can race cancellation; this is not atomic paint authorization. No cursor, hit testing, IME, input, wrap, font-lock or normal FrameView integration is enabled.

A permitted genuine platform Window remains required to execute and qualify this route. TestPlatform's NoopTextSystem would not establish native font/raster behavior and is not substituted here.

## Revised arithmetic boundary

Before invoking paint, the callback mirrors the pinned SDK unwrapped/undecorated line arithmetic in f32: both baseline subtractions, padding, baseline addition, origin-plus-baseline, each glyph-x delta and accumulated origin, and first-glyph-origin plus line width must stay finite. Repeated and nonmonotonic glyph positions remain allowed. This rejects the reproduced finite-input overflow cases, but is NOT a complete SDK geometry or ink validator. Native font bounding-box results, scale-factor/raster coordinates and backend internals remain outside this guard. Parent clipping does not provide an allocation/work or panic boundary.

The staged line is private and constructed with no underline/strike/background and no wraps. Changing those construction invariants requires revisiting this arithmetic subset. Neither receipt checking nor this preflight makes revocation atomic: the owner can revoke immediately after the final live check. SDK Err may follow partial scene recording; panic may also skip the SDK's ordinary content-mask stack pop. No rollback or unwind containment is promised.
