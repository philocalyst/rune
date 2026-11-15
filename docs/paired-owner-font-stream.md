# Experimental non-IME paired owner font stream

This adds an explicit library path, not a native CLI default or styled Org/Doom
claim. The default bridge/transport/view paths and public three-field
PublicationRevision remain unchanged. FrameDisplay protocol remains version 3;
font requests travel in a separate typed sidecar. The existing SVG path reads
FrameDisplay only and does not apply this sidecar, so font weight/slant SVG
parity is not established. Whole GNU default-font family
and geometry metadata remains unsupported by the owner projection, so this does
not enable ordinary fully bootstrapped package styling.

## Owner and publication

Create a font-plan mailbox, then use `bind_font_initialized` with a measured
plain capability record and an owner-only readonly projector. The production
adapter calls the authenticated persistent Lisp host query. No live Context or
Lisp value crosses threads. The callback must not evaluate Lisp or mutate the
owner; publication verifies the owner sequence again before committing the
prepared frame.

The worker compares exact plain font results (including errors) around ordinary
owner callbacks. A direct face-vector mutation can therefore invalidate a frame
without changing text/input epochs. Failed results remain comparison data, so
corrective evaluation still runs. An edit/evaluation is never replayed because
later font preparation or publication failed.

Prepared projections must match window/buffer, sequence, live logical owner-frame
identity, document publication epoch, visible range and exact capability record.
Runs must cover the visible range contiguously and request only available
variants. Bounds include 256 windows and assignments, 4096 rows/property runs,
65536 display clusters, and 100000 visible scalars per scope. Existing frame text
bytes and the cost of the host snapshot are not newly bounded by this feature.
Callback errors are limited to 4096 bytes at this boundary.

Each (original face ID, variant) pair receives a bounded presentation-local ID.
Only Face.id is remapped in newly prepared immutable rows. Source ranges,
window/buffer IDs, revision, geometry, selections and input authority are not
changed. A font transition inside a multi-scalar cluster is rejected rather than
splitting the cluster. The source scene is not mutated when shared by others.
These IDs do not identify Lisp objects or authorize input.

The latest-only font mailbox pins the exact Arc<FrameDisplay> with its plan.
It rejects stale generation and disconnected consumers. The main HostUpdates
channel provides wakes/capacity; consumers must not combine its frame payload
with a separately fetched sidecar. Publishing the font mailbox and publishing the ordinary frame/wake are TWO
NON-ATOMIC operations, in that order. A valid committed font pair can already be
visible when the ordinary publication later fails, including during initialization.
`latest().is_some()` is not initialization success: the caller must honor the
explicit initializer/lifecycle result and report partial delivery accurately.
There is no rollback of a committed pair or edit. Retry prepares/publishes a new
generation only; it never reruns the command.

Initial failures preserve the existing
close-and-drain accounting for queued, unprocessed OwnerEvents.

## UI admission

`FrameView::new_font_editor` validates the initial bank and plan before creating
the view and installs that initial plan without needing a later generation.
`watch_font_updates` consumes only the paired mailbox and drives the existing
live native admission gate. The stream fixes its bank identity: replacement is
unsupported. Ordinary present is rejected in stream mode, preventing accidental
normal-font fallback if the wrong watcher is installed. Fatal UI admission
errors stop the watcher and close input. Runtime reporting/presentation remains
the host's responsibility; this does not add a GUI error card.

Both transport and view reject mixing this mode with platform IME. No bank
replacement, preedit transition or new input/UTF16 policy is introduced.
Normal non-font constructors/watchers retain their previous behavior.

## Qualification status

The exact native --tests check passed. Four linked fixtures then executed and
passed in 0.06 seconds: real headless owner-thread/FIFO/mailbox/native admission,
style mutation after a Lisp error, repair/no replay, stale epoch/sequence/frame
scope and callback/initial-consumer failures, dropped-consumer committed-edit
preservation, and explicit paired-before-ordinary failure ordering at startup
and after an edit. Seven existing normal FIFO controls also passed on the same
linked ELF in 0.02 seconds. The tested source was root 7001b14 plus this patch;
these are not claims about later untested source or the whole native suite.

A real Application::headless can measure and admit fonts but cannot create a
Window. This change does not execute FrameView's entity constructor, watcher,
generation branch, TextRun painter, OS input, glyph rasterization or shaping.
The previous source/probe evidence is separate. No rendered weight/slant,
Unicode/grapheme/bidi, GUI workflow or GNU redisplay parity is claimed.

## Bounded grapheme font-transition preflight

The opt-in font projection now rejects a change of measured font variant inside
an extended grapheme, using Unicode segmentation 1.12 over the complete owner
buffer snapshot. Arbitrary visible substrings are insufficient context (notably
for regional-indicator parity). Each projected window is limited to a complete
buffer of 100,000 Unicode scalars and 1 MiB of UTF-8, in addition to existing
visible-range/run/window caps. Exceeding these limits is a visible projection
failure, not permission to skip validation. Work is bounded per window, not
cached or promised to be independent of total buffer length.

The exact snapshot sequence, buffer identity, publication epoch and scalar length
must match. Temporary endpoint variant extension supplies complete validation
coverage only; it never publishes styles outside the visible range. Equal-variant
administrative run splits are allowed. The guard rejects conflicting font changes
rather than moving scalar/UTF-16 offsets, caret positions or IME ranges.

This does not merge scalar display clusters, validate viewport clipping as a
grapheme boundary, shape text, establish Unicode cell widths or enable IME/font
mode. Current layout and native paint remain separate authorities. Tests of the
owner preflight are not evidence of a Window or text painter executing.
