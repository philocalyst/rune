# Opt-in retained-row FIFO lifecycle runner

No native CLI, FrameView or renderer enables this runner. Existing run and
run_with_outcome paths remain selected by production startup. There is no new
Lisp Env, Context, evaluator thread or mutable UI host. The new runner operates
on the existing NativeEditorWorker/H and invokes a service function with ONLY
&EditorBridge<H> and the plain RowPreparationOwner. It can snapshot owner state,
not access mutable host/evaluator authority through this interface.

A controller obtained from the single NativeEditorSender holds only Weak to
its input sender, so minting/retaining one cannot keep the LEGACY runner's
channel connected. Only the selected row runner upgrades/retains a scheduling
sender. It can enqueue only payload-free RowWake with an opaque privately
constructed marker and set cooperative-stop state. It cannot send keys,
source, saves or commands. Controller/worker identity is checked. Positive FIFO
capacity is required: capacity0 Full does not imply an existing queued turn.
Wakes coalesce; Full preserves pending and every queued owner turn services rows.
The pending bit clears BEFORE service so activity during service schedules anew.
Row closure is checked before starting a new data turn. FIFO data has priority
over nonterminal repeated row-ready activity. A row wait is registered
before idle receiver.recv and dropped before event processing/host access.
Callbacks/Wakers schedule a control event only; they never process owner code
inline. No Session/Entity/reply mutex is held over row service callbacks.

Stop is observed at turn boundaries, not an atomic interruption of a dequeued
command. A callback/native operation already running may finish; there is no
preemption or general bounded stop. Existing scoped QUIT requires separate
explicit use. Runner stop closes rows, closes editor admission, returns original
unprocessed DATA events once, and discards internal RowWake hints. A queued edit
is never replayed after callback/publication failure. Row callback errors are
reported separately and do not turn into a new evaluation or frame success.

An internal scheduling sender would otherwise keep recv connected after the
single UI producer disappears. Sender Drop therefore records input_closed and
wakes ONLY an enabled row runner; that runner exits and returns pending data.
Default runner sender-disconnect behavior remains unchanged. Dropping a control
handle alone does not request stop: application teardown must retain and call
an independent controller, as required by the activity lifecycle contract.

Draft tests cover idle registration/reply wake, actually parked producer Drop,
row-UI close, deterministic full wake after clear/service barrier with concurrent
stop, full control admission fallback, row errors without edit replay,
stop-before-start original-input return, independent positive-capacity wrong
controller/capacity0 rejection, initialization failure, legacy runner disconnect
while retaining a controller, and persistent Env GC/partial-error FIFO behavior. Test-only idle notices
are capacity-one nonblocking and compiled out of production.

## Focused qualification

The isolated source compiled with the root combined native tests/examples
configuration. Actual GPUI unit compilation used a separate test manifest whose
lib path points to this exact crate source, seeded from the combined root lock
and subsequently run locked/offline. All10 new FIFO tests passed; all64 GPUI
unit tests (including those10) passed. The actual root native binary ran the
persistent rooted Env/GC/partial-error test:1 passed. Test failure paths request
stop by completed-turn count even on an assertion regression, and final runs
were externally bounded. These are not a full root-suite qualification.

No production startup, native Window, shape, coverage, input geometry or paint
activation is claimed. The legacy default runner regression and typed opt-in
runner are tested; the real GUI still selects its original run_with_outcome.
