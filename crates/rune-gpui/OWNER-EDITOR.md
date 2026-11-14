# Recovered editor-host native input path

This is newly reconstructed recovery code, not a restored copy of the lost full
native editor snapshot. It uses official GPUI 0.2.2 with unchanged default
X11+Wayland features. The minimal default first-light fixture remains separate.

## Implemented boundary

- `EditorBridge<H: EditorHost>` owns exactly one command authority. Owned fallible
  snapshots come from host.snapshot(); dispatch, insertion, pointer placement and
  replacement go through the host. No borrowed/mutable Session escape hatch is exposed. Busy/closed owner query
  and prefix-cancellation errors propagate rather than panic or silently ignore.
- A host may be non-Send and borrow its sole runtime Context. Construct it inside
  with_runtime_owner's owner-thread callback, then construct/bind/run the bridge
  there. No Context enters the GPUI view or an executor task.
- Scoped composition carries stable window/buffer IDs, publication epoch and an
  exact scalar range. Owner-current-point paste captures that scope from one
  owner snapshot and invokes host.commit_replacement, not a Session-only bypass.
- `PreparedImeContext` holds only bounded owner-prepared surrounding text. Its
  origins must come from a verified Unicode index. Reads may expand surrogate
  interiors; replacements reject them. It does not authenticate numeric origins,
  inspect properties or scan a live document prefix from a platform callback.
- Composition tokens are process-stream unique. An emitted commit stays pending
  until its explicit edit acknowledgment succeeds or lifecycle cancellation occurs.
  A late/rejected outcome cannot silently retarget or recreate an insertion.

## Actual view/transport wiring

`FrameView::new_editor` consumes the single, non-cloneable UI sender and reply
handle. Keyboard, resize, source-mapped pointer events and submit_composition all
share one bounded OwnerEvent FIFO. No cloned sender can bypass an accepted view
backlog. A rejected or disconnected event returns ownership through a mandatory
callback; physical input pauses after saturation. C-g remains observable.

A HostUpdates wake takes explicit composition acknowledgments **before** flushing
queued input, freeing the reserved reply slot. A frame/capacity wake alone is not
an acknowledgment. Recovery of editor input uses take_pending_owner_input and
replay_owner_input, preserving the whole envelope order rather than extracting
keys around a pending composition.

`pending_editor_transport` creates UI endpoints without a host. The owner handoff
binds only in the owner callback. bind_initialized/run prepare and publish the
initial frame before processing queued input. Failure returns its cause and the
original undelivered requests; reject_unbound closes late admission under the
same lock as send, then drains ownership. Frame preparation/publication failure
is separate from edit success. retry_frame retries only rendering, never input.

The editor_first_light example constructs its pure Session only on the dedicated
thread, uses this actual editor-view constructor, and asynchronously waits for the
initial frame without a blocking UI receive or idle timer. Startup error state is
stored before final publisher drop wakes the UI. Its rejection callback fails
loudly; it is not a production recovery dialog.

## Verification and remaining gates

The dependency-light owner checker runs captured-scope, undo, stale epoch,
composition pending/late ack, UTF-16 boundary/context-limit, reply reservation,
startup rejection, edit-success/render-failure and FIFO/backpressure tests. These
are not native window tests. The exact recovered GPUI editor feature and original
first-light plus editor_first_light sources pass cargo check with the pinned lock.

No platform InputHandler is registered. There is no real preedit rendering,
platform selection/range geometry, or consumed-key/platform-text suppression
policy. A future handler must explicitly invalidate composition on every
focus/window/buffer/document/caret/selection transition, including ABA, and use
matching prepared layout/epoch for geometry. Full complex shaping and runtime
resolved-style/property-domain handling are not reconstructed here; paint uses
plain default faces. No live Lisp current-buffer gateway is connected.

No GUI/window/IME behavior has been verified in this recovered environment.
Native linking, actual font metrics, input/resize/close, runtime hooks/GC ownership
and real platform IME all remain separate gates.

Recovery manifests temporarily share rune-display and rune-editor from the
editor-recovery worktree to avoid duplicate same-version crate identities. Unified
integration must restore ../rune-display and ../rune-editor after merging the
owner API/publication prerequisites; do not publish recovery-machine paths.


## Owned gateway / platform-context checkpoint

The host interface now uses snapshot()->Result<SessionSnapshot<Doc>> and
cancel_prefix()->Result<()>; the renderer never keeps an Rc/RefCell borrow across
layout, hooks or a platform callback. Mode setup is fallible as well. Pointer
prefix cancellation belongs to the successful owner transaction, not a later
renderer call that could falsely reject an already-applied movement.

Editor-owned PlatformTextDocument supplies verified indexed scalar/UTF-16
conversions. bind_ime_initialized can prepare a bounded plain context from the
same owned epoch/caret as its frame, validate index roundtrips, and publish them
atomically in a native metadata mailbox before its normal UI wake. An edit
invalidates old writable context before any frame retry. This owner-side provider
is checked, but the platform EntityInputHandler/UI preedit adapter is still under
implementation and is NOT registered by this checkpoint.

Ordinary committed text has an ordered current-point event distinct from a
captured-range composition: queued plain typing does not wait for a single IME
ack slot or reuse stale scalar ranges. Explicit commands are likewise ordered.
These are protocol primitives; platform command-consumption and callbacks must
still choose exactly one insertion source before activation.
