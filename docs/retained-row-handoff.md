# Dormant asynchronous row preparation handoff

This is a plain-data, nonblocking adapter around the retained-row transaction,
not a GPUI producer or an enabled owner/render path. Capacity-one channels
carry bounded requests, numeric replies and explicit owner acceptances. There is one owner endpoint and
one UI endpoint; neither is Clone. It requires V2 transaction and checked-span
increments. No native font, ShapedLine, Window, Rc, Session or Lisp object is
sent. A real thread test exercises delivery and single-use correlation only.

The owner `prepare` reserves a request and tries to enqueue it. Busy admission
preserves the previous pending request. If the queue still contains canceled
work, Full cancels the just-reserved request, returns Saturated and does not
retry. ID reuse is never used to recover. The UI can drain canceled requests;
its later reply fails correlation. This bounded design needs a future wake and
retry scheduler; it is not latest-only coalescing or a GUI event loop.

The UI reads a request and returns its FULL key and opaque local lease number.
Reply saturation/disconnection returns ownership of the unsent plain reply.
The owner `poll` removes at most one reply and validates using fresh supplied
scope/source. Failure consumes that reply, never an edit. The caller must
supply the actual authenticated current owner snapshot and perform separate
geometry/native-lifetime checks. A successful correlation token is not a frame.

An endpoint-alive atomic detects an already-dropped UI before owner prepare or
poll, even when a reply remains queued. Detection closes the transaction and
revokes accepted leases. A UI drop racing AFTER that observation may happen
before/after completion; this is not atomic native-liveness validation. Existing
accepted leases are revoked when owner next polls/prepares or closes, NOT
synchronously merely by UI Drop. No paint authority should be inferred from
is_live. Owner Drop revokes immediately through transaction Drop.

Explicit owner close now marks the peer closed, drops request/acceptance
senders and the reply receiver, revokes work, and wakes both activity waiters.
UI request, receipt and reply operations reject an observed owner close; any
such UI observation drains its capacity-one request/receipt backlogs. UI close
marks its side closed and drains those backlogs too. Source already queued can
remain pinned until that UI observation or Drop. A racing operation may finish
before it observes closure; no atomic multi-mailbox delivery is promised. The
caller must discard native registry entries by full key on observed disconnect.

Tests execute actual cross-thread exchange, stale/late results, capacity-one
request/reply saturation, no hidden resend, queued replies from a dead UI,
owner/UI disconnection and accepted lease revocation. No Window, shape, paint,
Session integration or native coverage policy executes.

## Snapshot-preparation increment

`prepare_snapshot` captures a complete immutable TextSnapshot only when its
revision matches the supplied text epoch, source starts at zero, window/buffer
IDs are nonzero, and the whole source fits 64 scalars/256 bytes. It verifies
iterator length in both directions before request-state mutation. Failed
capture sends no request and does not revoke an existing accepted lease.
This is a bounded complete-buffer first slice, NOT viewport-sized work for
large buffers or full owner authentication. Sequence and selected-pair freshness
remain the owner caller's duty; snapshot acquisition and lifetime stay outside
the transport. Control characters still fail through ordinary prepare.

Owner poll now revalidates any accepted lease against its supplied fresh scope
and text BEFORE receiving a reply. Therefore an empty queue or unrelated late
reply cannot leave accepted geometry live after that call observes new state.
Callers still must invoke poll/revalidate/invalidate on owner changes; the
adapter does not subscribe to Session changes automatically.

## Explicit owner acceptance receipt

A third capacity-one channel carries the owner-issued AcceptedRowToken back to
the UI. Sending a reply is not acceptance: `next_acceptance` remains empty until
the owner freshly validates and consumes the reply. The consumer must correlate
the full key and local registry number, check revocation, and independently
validate native liveness and an exact accepted frame before presentation.
Receipt delivery is NOT frame publication, native lease proof or paint permission.
It may be observed before the caller processes `poll`'s return or publishes any
frame. No atomic receipt/frame/wake transaction is claimed.

Unread-receipt saturation cancels/revokes the newly accepted lease and returns
Saturated; it does not silently retry. An older queued receipt can still be read
but is revoked. IDs/full keys remain nonreused. Consumers must drain bounded
receipts or explicitly request new preparation later. This adds no wake scheduler.
The channel carries only an Arc-backed atomic revocation flag and plain IDs,
never native or Lisp values.
