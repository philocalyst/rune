# Bounded contextual-row structural admission

The pure `row_shape_admission` module validates producer-supplied glyph positions
and coverage verdicts. It does NOT establish that a producer shaped these inputs,
that its font key belongs to the current app, or that a coverage assertion is true.
Those remain trusted producer responsibilities. No normal renderer uses this API.

The first slice accepts nonempty complete text <=64scalars/256UTF-8bytes, limited
to non-control codepoints <=U+036F (including some format characters), not a
Unicode printable-character category. Named source/glyph limits are exposed by the
module; whole-text grapheme enumeration is linear within those limits. It checks one glyph per extended grapheme, exact source-byte starts,
one expected opaque font key, finite positive forward advances, leading x=0 and
y=0. Ligatures spanning multiple graphemes, multi-glyph marks, emoji, RTL/other
scripts, controls, fallback keys, unsupported offsets and oversized rows reject.
This is advance geometry, not ink bounds or a universal shaping subset.

Coverage is explicit tri-state. Missing AND Unknown fail closed; Unknown is the
default. VerifiedAvailable is a producer assertion, not derivable from a scalar
advance sum or font-family match. Output contains only exact scalar ranges and
contextual advances. Origin/viewport/caret numerical safety belongs to the existing
measured owner-layout admission, not this relative-coordinate helper.

Eleven focused structural tests execute, covering exact offsets, tri-state coverage,
unsupported mappings, float edge cases and each named bound. A separate independent cosmic-text probe
feeds real public glyph positions/IDs into this same helper: AVATAR and combining
a+acute+b were admitted; ffi was rejected, and changing the accepted verdict to
Unknown caused rejection. That proves this independent path, NOT GPUI authority.

## Actual GPUI public-API limitation found during implementation

A real-Window row-producer draft can call public shape_line for a complete row,
retain that SAME ShapedLine and live bank for eventual painting, and compare actual
run font IDs. But GPUI0.2.2 GlyphId's numeric field is private and it exposes no
public missing-glyph/availability accessor. Reading it correctly fails compilation.
No Debug parsing, transmute, private-field construction or vendor workaround is used.
The draft remains explicitly WIP and is not exported or included in this patch.

This limitation blocks the proposed verified-coverage contract; it does not mean
GPUI cannot shape or paint ordinary text. A separately reviewed explicit
replacement-possible policy or a supported availability API could change that
contract. Neither is silently substituted here. Public real headless full-row
shaping remains unavailable without WindowTextSystem; Window and paint execution
have not occurred. Existing renderer and diagnostics remain unchanged.
