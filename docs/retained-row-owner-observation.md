# Explicit owner observation before a preparation reply

The committed handoff revalidates accepted leases on poll, but a pending request
could remain live after an edit/point/style/viewport change until a reply arrives.
The new explicit `observe_owner_state` hook compares fresh supplied scope/source
with BOTH pending and accepted state. A mismatching pending request is removed
and its shared flag revoked, so the UI's bounded live drain can discard queued
work before shaping. A matching request remains pending. The return reports
pending_revoked and accepted_live; it never prepares replacement work or replays
an edit. Keys remain nonreused. Existing poll error semantics are unchanged.

This hook is not called automatically: an eventual owner-turn integration must
supply genuinely current selected window/buffer, sequence, text epoch, style,
viewport and bank/policy revisions. Numeric equality is not authentication.
Owner endpoint loss closes instead of recommending rescheduling. A canceled
request may still occupy the mailbox until UI drain; explicit retry can return
Saturated and is not silently resubmitted. Revocation can race UI work and does
not constitute atomic shape/paint cancellation.

Actual tests cover unchanged preservation, sequence ABA, exact-source mismatch,
accepted revocation, closed-state behavior, a separate UI thread draining queued
revoked work, endpoint loss and a real Session<RopeDocument> point-away/back
sequence with unchanged text epoch. The real owner test verifies no observation
side effect on text, point or sequence and explicitly prepares a fresh request.
No production native event loop, Window, shaping, paint or automatic wake runs.
