# Owned-child process lookup and signals

`get-buffer-process` resolves a current buffer (nil), existing owner-local buffer
name or buffer object, then scans rooted process snapshots. Missing names return
nil without creating a buffer. A matching live record returns the genuine
process object, not its PID or a fabricated token. Owner checks and rooted
snapshots remain in effect across moving collection. Only Running associations
are selected; complete GNU dead-process/retired-registry metadata parity is not
claimed.

`signal-process` is deliberately narrower than GNU's arbitrary PID facility.
It accepts only a genuine owner process/name or a PID found in this Context's
registered child snapshots. Unregistered PIDs, process groups and unsupported
signals are rejected without an OS signal. Supported Unix signals are SIGINT,
SIGKILL and SIGTERM (2, 9, 15); non-Unix platforms reject this facility.

Delivery is requested through a 16-item native control queue, never by passing
an arbitrary PID to libc from a Lisp wrapper. The supervisor exclusively owns
its unreaped Child and performs the checked positive-pid `kill` syscall before
any reaping, preventing PID reuse from redirecting a signal. A one-shot reply
acknowledges the actual syscall result; acknowledgement wait is capped at
100 ms. Timeout means delivery is unconfirmed, not a promise that it did not
happen, and must not be blindly retried. GC finalizers still use nonblocking
termination, not this acknowledged signal API. No Lisp values or callbacks
enter native workers. Signals do not extend the direct-child-only termination
scope or claim full GNU stop/continue/process-group semantics.

Synthetic tests prove a real Python SIGINT handler exits with its chosen code
42 (not a fake SIGINT-to-SIGKILL mapping), dead/unsupported signals error, and an
empty owner registry rejects the runtime PID without dispatch. The test never
signals an account, unrelated application or real mail backend.
