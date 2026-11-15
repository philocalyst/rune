# Actual GPUI foreground activity await with controlled shutdown

This example is an isolated driver, not production GUI activation. It uses the
committed executor-neutral activity Future with a real GPUI foreground task and
an independent real Session<RopeDocument> owner thread. No Window, native row,
coverage, contextual width, input geometry or paint is constructed.

Before permitting the owner to run, the foreground task polls its ACTUAL wait
future once with the real task Context and requires Pending. Only then does a
capacity-one bootstrap channel release the owner. The SAME future is retained
across await; request or owner-close activity must wake that registered task.
This is not a manually triggered replacement UI wake. On the owner, the same
activity Future uses a scheduling-only Thread::unpark Waker and a bounded
park_timeout loop. A wake-before-park token prevents lost registration.

The UI owns the plain endpoint outside an Entity and drains BOTH request and
receipt queues before every subsequent wait. It checks the full owner receipt
key and diagnostic token42, then explicitly closes. The owner observes that
close, revokes its accepted correlation and asserts unchanged point/sequence.
This one-row fixture does not define a general multirow drain/retry scheduler.
No owner edit is replayed, and the fixture token is not a native resource.

## Independent lifecycle control

The separate owner thread checks a two-second deadline when activity is pending
and closes through endpoint Drop on every return/error. Continuous Ready activity
could delay that check, so this is not a flood-proof scheduler. It does not require the UI
future or last task handle to be destroyed to run that close. Thus a retained
task/Waker graph has a reachable, independently executing opposite-endpoint
controller. Foreground startup stalls or arbitrary runtime panics are not
contained by this mechanism; every executable run is also externally bounded
by timeout10s. Owner construction and ordinary OS scheduling are not claimed
hard real-time operations.

Executed gates on the exact source:

* Normal: exit0, registered foreground wait resumes from request activity,
  owner accepts and UI verifies receipt, UI close wakes owner, lease revoked.
* `--close-before-request`: exit0, owner terminal close wakes the already
  registered foreground task without publishing any request.
* `--suppress-reply`: exit1, UI parks without replying; independent owner
  deadline/drop closes and wakes it. The external timeout was not reached.

## Callback and executor boundary

No Session mutex, Entity update guard or controller lock is held over endpoint
calls. The owner Waker only unparks its thread. For the pinned GPUI0.2.2 Linux
implementation, ForegroundExecutor::spawn schedules via
PlatformDispatcher::dispatch_on_main_thread; LinuxDispatcher sends the runnable
to its main queue rather than polling UI code inline. This source audit and
these actual runs qualify only this concrete driver/executor, not arbitrary
Wakers or other platforms. Endpoint operations can still synchronously invoke
arbitrary caller-provided Waker callbacks outside their own mutex; caller-lock
safety remains a driver obligation.

The example does not establish Entity-owned waiter extraction, production
native event-loop wiring, actual future cancellation policy for every executor,
automatic task reclamation, native Window/shape/paint or Org styling. Existing
normal renderer and CLI behavior remain unchanged.
