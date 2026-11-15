# Bounded stale-work draining before native staging

The UI endpoint gains opt-in `next_live_request` and `next_live_acceptance`.
Each consumes at most ONE item from its capacity-one mailbox and returns None
if that item's shared cancellation bit is already revoked. It does not scan,
wait, retry an owner command or fabricate new work. Raw receive methods remain
available for exact error/lifecycle diagnostics.

This frees the request slot after canceled work and the receipt slot after a
revoked acceptance without forwarding either into native staging. Owner retry
is still explicit: draining does not automatically enqueue a replacement. None
means no currently live item observed, not necessarily an empty queue before
the call or a still-connected endpoint. The symmetric-close increment rejects
observed peer closure before delivery and drains bounded backlogs. Closure can
still race the observation; an in-flight operation is not an atomic close barrier.

Cancellation may race the filter. Callers must recheck request state at native
preflight and correlate a live owner receipt/accepted frame later. These methods
provide no atomic paint authority, native glyph coverage or automatic wake.
They are not wired into the production GUI. The headless Entity diagnostic
uses them for its normal valid exchange and still proves owner-observed
payload-drop disconnect, not native Window behavior.

Actual tests cover canceled request draining on a separate thread followed by
explicit retry, and revoked receipt draining followed by a fresh accepted key.
The real headless GPUI Entity diagnostic also ran normalexit0 and held-entity
intentionalfailure1 with the live filters. No Window, actual shape, paint or
native staged-row resource destruction executes.
