# Dormant rooted owner-job foundation

This module is compiled and tested, but has no production call site. It does not change
fset, symbol lookup, go, channels, Context collection, the native owner FIFO,
bootstrap, or scheduler semantics. Existing owner-loop integration remains a
separate gated change. The copied admission state machine is a bounded reference
component, not another enabled runtime event loop.

## Ownership / shutdown contract

A JobRegistry is owner-thread-only (Rc), stamped with the exact Context domain
and read-only OwnerLiveness witness. Native Sender holds only IDs/cancel intents
inside the admission mutex, never a Context, expression or root. These IDs do
not authorize heap access. Each JobRegistry::new currently mints a distinct
admission RuntimeId: multiple registries for the SAME Context can therefore have
different native IDs. These are admission-instance identities, NOT proof of one
canonical Lisp runtime membership. A future owner driver must explicitly bind
and enforce its canonical runtime/registry/Context association before adapting
cross-thread job submission or function resolution. This foundation does not
enforce that association. Runtime identity and job sequence are checked and
not reused; capacity counts records until terminal owner reaping. Ready FIFO
order is commit order, not reservation order.

`unsafe enqueue_owned` requires the COMPLETE reachable graph to belong to that
Context, or satisfy the established global-sharing contract. The domain check
only authenticates the destination; it does not inspect payload provenance.
It reserves map/native capacity, creates a rollback reservation, registers an
existing HeapDomainValue root, then commits Ready. No callbacks or collection
occur in this staging window. Reservation Drop cancels/reaps an unpublished
job on ordinary unwind. Allocation failure/mid-GC panic recovery is not claimed.

Dequeuing transfers the independent Arc payload root OUT of the queued map into
a RunningLease. The lease also owns Rc<State>. Registry close/drop closes
admission first and retires queued roots only. Running remains tracked with a
cancel-request flag until lease completion/abandonment, even if the original
registry has been destroyed. No State -> RunningLease back edge exists.

RunScope borrows its lease. Safe Rust cannot consume/drop that lease while the
scope is active. Bind validates the exact live owner before accessing the Hdv
root and releases its short root-store lock before returning. It does not hold
a registry/admission/RefCell guard over evaluation or GC. An existing active
scope may bind its retained payload for unwinding after admission closes;
entering a NEW scope after close/cancellation is rejected.

This scope is NOT an evaluator or an authorization to ignore ordinary GC root
rules. The future owner driver must create/root the job Env and expression/frame
values in a scoped call and unwind ALL active frames BEFORE dropping RunScope
and consuming the lease. A scoped mutable Context execution borrow prevents
Context destruction during that actual execution. The current foundation does
not implement that driver, dynamic binding isolation, cooperative waiting or
result delivery. It cannot prove interpreter Rust-panic recovery.

If a native lease outlives its Context, existing Context retirement closes all
registered Hdv stores before heap release and retires OwnerLiveness first. The
escaped lease cannot enter/bind the retired heap; its later native destruction
does not dereference old Lisp memory. It is not a way to extend Context lifetime.

Root retirement/removal occurs outside admission and queued-map guards. Actual
native payload finalization still happens through the established collector and
deferred finalizer ordering, not on a cancellation sender thread. Cancel intent
alone is not immediate root retirement; the driver MUST arrange an existing
owner-FIFO wake to reap cancelled jobs at an owner boundary. No wake is wired
in this dormant slice.

## Cycles and failure boundaries

Registry owns queued roots; running lease owns its own root and State; native
Sender never owns State. State has no lease back-reference. Future Lisp job
handles must contain IDs/witnesses only, not a strong root/registry/lease, or
this proof must be revisited. This does not repair independent native self-root
issues elsewhere (e.g. the held completed Process design).

Cancelled/abandoned state is native bookkeeping, not rollback of committed Lisp
edits, messages, function publication or I/O. The native panic fixture tests only
scope destruction and explicit abandonment after ordinary Rust unwind. Poisoned
admission produces an error; Drop is best-effort native cleanup, not a recovery
protocol for a corrupted interpreter. No arbitrary Rust panic/OOM claim.

## Historical qualification before cloud loss (NOT current execution)

Before the loss, `cargo check --tests` passed. Focused tests: 19 PASS (13 native admission tests
plus 6 rooted lifecycle tests). The recovery adds staged-cancel, duplicate-enter,
live foreign-domain gate and commit-order test sources. In the recovered
workspace, all 23 focused tests passed. The rooted tests cover queued/running moving-GC
payload retention, Registry shutdown while RunScope is active, escaped lease
after owner retirement, queued native endpoint finalization once, ordinary native
unwind without Rc retention cycle, and reentrant admission/cancellation/GC with
no outstanding registry borrow. Full suite and independent review are separate
requirements before any integration. No bootstrap success is claimed.
