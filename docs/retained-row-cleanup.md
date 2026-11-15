# Pending cancellation and bounded native staging cleanup

The previous lifecycle signal began only after owner acceptance. An unaccepted
UI-staged row could therefore remain retained without any receipt telling it
that the owner canceled. Requests now carry a shared atomic revocation bit.
Request clones observe cancel, transaction Drop/close, matching stale completion
or invalid zero token. Successful acceptance reuses that SAME bit, so accepted
receipt invalidation also invalidates the retained request. Unknown keys do not
revoke a different pending request. No canceled bit is ever set true again.
This is loaded-module local observation, not authentication or atomic paint.

Native staging preflight now rejects canceled requests after checking actual
bank/App baseline authority. `StagedNativeRowSlot` retains at most ONE private
staged native row. Replacement drops the former row; clear drops it explicitly;
prune drops it on observed request revocation or bank/App expiration. No
ShapedLine or paint getter is exposed. Replacement with an invalid row fails
before replacing the current row; caller must still prune/clear stale current
state independently. These methods do not run automatically or schedule wakes.

## Executed versus compiled

Actual pure transaction regression covers cloned pending cancellation, matching
errors, Drop, successful acceptance sharing the request bit, and subsequent
accepted revocation. Existing async tests also run on the changed request type.
Actual native headless preflight rejects a canceled pending request with the
current valid bank; a separately LIVE request tests distinct-bank rejection.
No real Window shape, StagedNativeRow allocation, singleton replace/prune/clear,
retained glyph destruction or paint method executes in this environment.
Those native slot operations are compiled only; no runtime native cleanup is
claimed. The earlier genuine Entity endpoint-drop diagnostic is a different
executed lifecycle path and must not be substituted for native row destruction.

Consumer integration must observe cancellation on owner/UI wakes, app teardown
and before any eventual presentation. Atomic is_live reads can race later
revocation; explicit exact frame/lease correlation, live Window validation,
coverage/fallback and synchronized input geometry remain prerequisites. Backend
shape panic/OOM and post-shape resource bounds retain their prior caveats.
