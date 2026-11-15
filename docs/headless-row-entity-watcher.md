# Headless WeakEntity watcher and independent cancellation controller

This example executes real GPUI foreground scheduling, Entity lifetime and
retained-row activity transport on ce4c31d. It does not construct a native row,
Window, font shape, input geometry or paint. The Entity stores a receipt count
and an actual payload-Drop flag, not a fake rendered frame.

The plain UI endpoint belongs to a foreground watcher task OUTSIDE the Entity.
The watcher holds only WeakEntity and upgrades it inside a short App update
to project a validated owner receipt. No strong Entity handle or Entity guard
survives an await. A separate foreground controller owns the strong Entity.
It waits until the watcher's SAME real activity future is polled Pending and
registered, then drops the Entity, runs a genuine App effect/update cycle and
signals a separately executing owner thread through AtomicBool+Thread::unpark.
The owner closes its endpoint; terminal activity wakes the watcher, which
confirms WeakEntity no longer upgrades and observes mailbox disconnection.
The controller separately confirms actual Entity payload Drop occurred.

The owner thread owns a real Session<RopeDocument>. It checks cancellation and
its absolute two-second deadline on EVERY owner loop iteration, including when
activity is continuously Ready, before polling replies. Its endpoint Drop
closes/wakes on every error path. This controller survives a retained UI
Task/Waker graph and does not depend on that task's Drop to run shutdown.
Normal UI operation drains both applicable mailboxes before waiting. No Session,
Entity or controller mutex guard is held over signal-producing endpoint calls.
The owner Waker only unparks; the pinned GPUI Linux executor schedules the
foreground runnable rather than polling it inline, as audited previously.

## Executed modes, each externally bounded by timeout10s

* Normal exit0: full owner receipt projected once; watcher registered Pending;
  controller drops Entity, cancels owner; payload Drop and WeakEntity death
  separately observed; owner revokes without changing Session point/sequence.
* `--queued-receipt` exit0: after owner confirms receipt publication, watcher
  DELIBERATELY leaves that receipt queued and parks. This is a cancellation
  backlog fixture, not normal drain policy. Controller drop/cancel wakes it;
  closed-mailbox observation drains receipt WITHOUT projecting it (count0).
  No additional preparation or acceptance is attempted with that backlog.
* `--keep-entity` exit1: intentional negative control retains the strong Entity
  while closing the owner; watcher detects it still upgrades, then cleanup runs.
* `--suppress-cancel` exit1: Entity drops but explicit controller signal is
  withheld. Independent owner absolute deadline closes/wakes; the resulting
  owner deadline error fails the diagnostic. No external timeout was reached.

The initial watcher activity future is also registered Pending before owner
startup, avoiding an accidental all-ready/no-wake success. Bootstrap and phase
notifications coordinate the fixture only; they do not replace row activity
wakes. All per-phase channel waits are bounded (three seconds). Foreground
startup stalls, backend panic/abort, unrelated synchronous work and scheduler
starvation are not covered by a hard real-time guarantee; the external watchdog
remains necessary. The example does not prove every executor's cancellation
policy, general multirow fairness, a production Entity listener abstraction or
native staged-row cleanup. Normal renderer/CLI behavior stays unchanged.
