# Symmetric logical shutdown and coalesced activity

This isolated increment remains dormant. Owner close now publishes a shared
owner-closed flag, revokes its transaction, drops its request/acceptance senders
and reply receiver, and closes both activity streams. UI receipt-only polling
therefore observes explicit owner close rather than waiting indefinitely for
owner Drop. Replies after observed close fail and retain the caller's reply.
UI close/Drop publishes its own flag, drains its bounded receive queues and
closes both activity streams. Owner close/Drop and UI close/Drop are idempotent.

This is logical shutdown, not an atomic cross-thread barrier or eager global
memory reclamation. A request already queued in a live UI receiver can retain
source until UI observes disconnect (through any request/receipt/reply method)
or closes/drops. Observation drains at most one item per capacity-one queue.
A producer racing close can finish before seeing the flag. UI close wakes the
owner, but accepted revocation still requires owner observation/poll or close;
it is not automatically synchronous with UI close.

Each endpoint exposes `wait_for_activity(&mut self)`. Its cancellation-safe
Future borrows that endpoint and yields Ready or terminal Disconnected. Activity
coalesces into ONE bit: requests, replies, acceptances, capacity restoration and
explicit invalidation can trigger it. A wake means inspect current queues/state,
not one message, guaranteed work, acceptance or paint permission. Spurious wakes
are allowed. On Ready, drain applicable bounded queues then register again.
On Disconnected, close/observe on the owner before treating leases as invalid.

Registration and signaling use the same short mutex, preventing the classic
check-then-register lost wake. Waker clone, replacement Drop and wake all occur
outside that lock, including reentrant callback paths. One waiter per endpoint
is enforced by the borrowed future. Dropping a wait unregisters ONLY its own
checked-generation registration and drops its Waker outside the lock; it never
consumes the ready bit or payloads. No callback, Session or Lisp borrow is
held across a wake or caller code. These streams carry no native resources.

A foreground task may OWN the plain UI endpoint and await its activity, then
update a native Entity after the borrow ends. An endpoint stored INSIDE an
Entity cannot return a borrowed wait future out of Entity::update; that layout
needs a separately designed watcher/listener split. This increment does not
pretend to provide that split or production GUI wiring.

Tests cover signal before/after registration, repeated coalescing, cancellation
and Waker replacement with reentrant Drop,64 concurrent registration/signaling
interleavings, terminal/idempotent close, request/reply/receipt/capacity wakes,
receipt-only close observation and bounded source release. No native Window,
shape, paint or normal editor event loop is activated.


## Required external lifecycle ownership

Actual Future::drop now unregisters promptly, but dropping an external task
handle is NOT proof that the executor dropped its future. A still-pending
registration's opaque strong Waker can keep a task alive when the task owns
its endpoint (task -> endpoint -> activity -> Waker -> task). An executor-neutral
API cannot downgrade an arbitrary Waker or force executor cancellation.

An independently owned lifecycle controller MUST close/drop the opposite
endpoint, explicitly close an externally reachable endpoint, or provide a
verified executor cancellation mechanism that actually drops the future.
Do not rely on endpoint Drop or incidental future messages to break a cycle
which prevents that Drop. No automatic reclamation is promised. A regression
models a task owning the UI endpoint and confirms that removing external task
references leaves it retained while its wait remains pending; opposite-owner
close releases the Waker and task. Separate tests prove actual future Drop
releases its registration and an old canceled future cannot erase a newer one.
These are deterministic ownership tests, not proof for every executor's task
cancellation policy. Registration-ID exhaustion closes activity without reuse.
