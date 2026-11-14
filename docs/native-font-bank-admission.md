# Measured native font-bank admission prerequisite

This is an opt-in diagnostic/API prerequisite. Native startup and FrameView do
not construct or use the bank. No bold/italic rendering is enabled.

The UI-owned bank retains at most four normal/bold/italic/combined measured
faces. Native Font/FontId values never cross into the editor or layout worker.
A weak TextSystem lease rejects expired and foreign contexts. The bank is not
Send or Sync. Each bank receives a checked nonzero correlation ID; that ID is
not an input authority or security credential.

The existing plain rune-display capability record reports measured availability
and baseline geometry. `get_compatible_font` first validates the live TextSystem,
then requires the prepared bank ID, finite and exactly equal baseline geometry,
normal availability and requested variant availability. Only then does it
return the UI-owned native descriptor. Raw `get_font` checks lifetime/availability
but not prepared-frame correlation; a future painter should use the combined
method rather than treating a true availability flag as permission to paint.

Representative 0/i/m advances must be compatible with the baseline. Variant
ascent/descent must fit its fixed-grid vertical budget. Distinct resolved IDs
avoid claiming that a fallback to the same face established a new style.
These checks do not prove exact requested weight/family, Unicode-wide advances,
glyph coverage, ink bounds, grapheme composition, ligatures, shaping or bidi.
GPUI's official resolve_font may panic if no requested or fallback font resolves;
this wrapper does not promise recovery from that upstream case.

## Actual headless evidence

On Linux with the official locked GPUI 0.2.2 dependency and installed DejaVu Sans
Mono at 17 logical pixels, the real `Application::headless` diagnostic admitted:
normal FontId(3), bold FontId(0), italic FontId(2), combined FontId(1). It rejected
a stale bank ID and a changed baseline advance, then exited through the shared
queued foreground quit helper. Exit status was zero. Different font systems may
report unavailable variants; the diagnostic reports those explicitly and only
requires the normal face. It opens no Window and calls no shape_line or painter.

The exact frozen source snapshot was root 6b7bbe6 plus the bank and diagnostic
patches, not later startup/Marker changes. Native `--tests` compilation passed;
seven geometry and one ID-allocation helper tests executed. The diagnostic
linked in 6.51 seconds and ran in 0.10 seconds. The combined gate's expired or
foreign TextSystem error paths have source review, not direct runtime coverage.

Run the optional diagnostic with the native feature:

```
cargo run --offline --locked --features native --example headless_font_bank_gate
```

This headless result is not OS input/window, raster paint, FrameView styling,
Org/Doom workflow or GNU redisplay parity. Integration still needs actual
protocol bank/style fields, bounded owner-to-UI capability agreement, style
publication and frame admission, explicit bank replacement/IME transitions,
then real glyph/window verification. Current scalar clusters are not graphemes;
there is no implicit permission to split graphemes or snap exact Lisp/IME offsets.
