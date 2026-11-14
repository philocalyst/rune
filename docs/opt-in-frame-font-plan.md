# Opt-in per-face font admission seam

This prerequisite adds an explicit API, not automatic GNU face-weight publication.
Existing constructors and ordinary `present` stay normal-only. No normal startup or
command path calls the new method. A separate experimental paired stream can
consume owner requests explicitly; see paired-owner-font-stream.md. No default style or display protocol field
changes. `FrameDisplay` remains protocol 3; the optional typed sidecar is separate.

A plain `FrameFontPlan` pins one exact immutable `Arc<FrameDisplay>` and maps
existing `Face.id` values to measured normal/bold/italic/combined requests.
The caller must supply that mapping deliberately. Omitted IDs mean normal.
Duplicate or absent IDs are rejected. Plan validation caps assignments and
windows at 256, rows at 4096 and clusters at 65536 before further traversal.
These are structural validation bounds, not a total text-memory or paint budget.
Exact Arc identity cannot be replaced by another frame with equal generation.

`NativeFrameFontPlan` holds a UI-only Rc bank. Creation validates the base font
family/features/style, size, exact metrics and measured advances, live TextSystem,
matching bank correlation and every requested variant. `present_font_plan`
requires a strictly newer frame and validates the view baseline before atomically
replacing frame and plan. Existing normal `present` clears any previous optional
plan on a newer frame. No state is changed on rejection and no edit is replayed.
Platform IME views reject the opt-in plan entirely until a separate bank/style
transition policy is implemented. This API does not alter input authentication.

The native TextRun branch selects a descriptor through the live combined bank
gate for the face ID. Failure skips the text and reports once per frame rather
than silently substituting normal. Geometry remains the measured baseline grid.
The bank is immutable after admission, and selected native Font/FontId values
never cross into the owner/worker. No Lisp values are stored in these types.

## Executed versus unverified

Copied exact display-library tests passed, including exact Arc correlation,
invalid metrics/zero bank, duplicate/absent bindings, requested selection and row
bounds. Native `--tests` compilation passed. The official headless diagnostic
then exercised actual NativeFrameFontPlan creation and descriptor lookup for all
four measured variants; stale bank IDs, changed metrics and a different 18px
baseline were rejected. It checked the exact-frame identity predicate against
an equal-data but different Arc. The `equal_data_arc_not_matched` output labels
only that identity predicate, not presentation or generation rejection. This is not execution of FrameView's entity
presentation or strictly-newer-generation branch, which remains compile/source
reviewed only. Shared queued quit completed with exit zero.

No Window was created and the painter was not executed. No glyph raster/ink,
shape_line, grapheme, ligature, bidi, OS input, IME, automatic GNU font mapping,
Org/Doom styling or GNU redisplay parity is established. The current fixed-grid
scalar layout and representative ASCII font-bank limitations remain unchanged.
