# Opt-in native platform input

`FrameView::new_ime` now implements GPUI 0.2.2 EntityInputHandler and registers
ElementInputHandler during each canvas paint with the actual element bounds and
focus handle. This is an implemented/compiler-checked adapter, not a claim that a
native IME or window was exercised.

## One authority, matching context and paint

Create platform_frames() alongside the host-free pending owner channel. Inside
the owner callback, construct the bridge and call
owner.bind_ime_initialized(bridge, platform_publisher). The bridge's host uses
fallible owned snapshots, never a retained RefCell guard or borrowed Session.
PlatformTextDocument supplies indexed scalar/UTF-16 conversion. Only a bounded
plain text segment, selected caret and matching frame cross to the UI, not a
Context, live document, property lease or raw Lisp value.

The owner publishes matching frame/context atomically before its normal wake.
An edit invalidates old writable context before frame retry. Identity includes
window, buffer, text publication epoch and owner sequence; owner-entry validation
rejects buffer/window ABA even if coalescing ends at the same IDs/text epoch.
Ordinary FIFO current-point text is deliberately distinct from a captured-range
composition. Never resend an enqueued scoped request because its outcome is late.
This is not durable reconnect deduplication or deduplication of unidentified OS
callbacks with identical text.

Use the actual shared evaluator/runtime gateway host when connecting Lisp.
Constructing a second Session would create a second authority. The included
ime_first_light uses a pure Session fixture, not the live Lisp gateway, and makes
no claim about actual runtime hooks/GC execution.

## UTF-16, preedit and outcomes

All platform ranges are document UTF-16. Reads may expand a surrogate interior
and return the adjusted range; replacements/selection boundaries reject interiors
rather than rounding. The surrounding context is bounded, owner-indexed and
prepared near the caret. Requests outside it or outside visible geometry return
unavailable or an explicit failure callback, never invented text/coordinates.

Preedit changes are UI-only bounded virtual replacements. They never mutate the
owner buffer per keystroke. The UI prepares a floating composition panel outside
paint, up to four visible rows; this is not speculative full-document reflow or
full complex-script shaping. Geometry queries use that panel or the matching
prepared frame and return None where exact geometry is unavailable.

Final replacement emits one captured-scope request through the same bounded FIFO
as ordinary input. Unmark commits the current draft once; repeated unmark after
emission is a no-op. Emitted preview remains pending until explicit edit success
or lifecycle cancellation. Rejection keeps it for resolution. A frame/tombstone
is not a success acknowledgment. Edit success and later frame/publication failure
remain separate; retry_frame never reexecutes an insertion.

Focus, activation, pointer/bounds changes and published scope/selection changes
cancel or suspend composing input. Explicit cancellation after enqueue does not
roll back an already accepted owner request. NativeImeFailure carries the failed
operation/payload to a mandatory callback outside state borrows. Transport
rejection separately returns original OwnerEvent ownership for explicit recovery.

## Exactly one printable source

IME mode lets printable keys reach the platform only. It uses explicit native
navigation/edit commands (arrows, Home/End, delete/backspace, Enter/Tab, undo/redo)
and consumes them before platform fallback. While composing, candidate-navigation
keys go to the platform. C-g/Escape explicitly cancel local composition.

This mode does not guess asynchronous keymap-prefix consumption. Typed C-x
prefixes and shift-selection are explicitly unsupported here; the existing
non-IME editor_first_light retains its typed keymap mode. Choose a mode when
constructing the view/owner; do not switch insertion sources mid-composition.

## Verification and remaining gates

The no-GPUI checker tests virtual UTF-16 edits, replacement expansion, exact
boundaries, pending/rejected/late ACKs, coalesced owner ABA, geometry unavailability,
ordinary typing FIFO, startup failure, saturation and one-undo owner integration.
The real official-locked GPUI checks include ime_first_light, editor_first_light
and the default first_light. No native linking/window/OS IME behavior is claimed
by these checks. Candidate placement, compose/cancel behavior per platform,
focus/resize/release races, unsupported-context UX and real font/shaping parity
still require a supported display executor. Do not bypass socket restrictions to
claim that verification.
