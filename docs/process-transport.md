# Native subprocess transport (mu prerequisite, not complete process support)

`process_transport` contains no Lisp/GC values and performs no callbacks.
`SpawnSpec` is a program plus argument vector, optional cwd and environment
overrides; it uses `std::process::Command`, not a concatenated shell command.
The caller owns authorization, executable selection and environment policy.
The mu benchmark passes an explicit synthetic HOME/XDG directories and mu DB.

`ProcessTransport::spawn` returns an Arc residence. Output events retain exact
bytes, including NUL/FE/FF/non-UTF8. Stdout and stderr remain separate. The owner
must decide coding and dispatch filters/sentinels on the Lisp owner thread.
`status` is Running/Exited(code/signal), not a PID-identity test. GC wrappers must
use their own checked identity and owner domain, not reusable OS PIDs.

An Exit event can precede the last output/EOF event. The owner pump must drain
both output EOFs before delivering the final sentinel, recording its delivered
transition before callback evaluation to handle reentrancy safely.

## Lifecycle and limits

Four bounded workers handle supervisor, stdout, stderr and stdin. The supervisor
owns/reaps the direct Child, including startup error cleanup. Last Arc drop or
`terminate` requests cancellation without wait/join in a collector/finalizer.
The direct child is killed and reaped by its supervisor. This is **not** a
portable descendant-process-tree killer: descendants retaining pipes can delay
EOF and retain reader workers. Global worker capacity (64) still bounds that
case; at most 16 unreaped direct processes can be admitted.

Arguments/environment: at most 256 each and 64 KiB total. Input: at most 8192
bytes per atomic enqueue, 32 queued chunks; full queue signals WouldBlock and
nothing from that call was enqueued. Output: 64 queued data chunks, reads of
8192 bytes, 64 MiB cumulative output per process before cancellation. Individual
`recv` waits are capped at 100 ms. EOF/exit/failure controls use a separate
terminal mailbox (at most ten bounded records by construction), so a full data
queue cannot retain workers or process slots after cancellation. Output enqueue
checks cancellation while backpressured; pending data is drained before observed
terminal controls. Queued data can remain retained by a surviving native Arc,
but direct-child workers finish without requiring the owner to drain it. Native/thread creation and OS operations can
fail; this is not a universal peak-memory or hard real-time scheduling promise.

This module alone does not implement Emacs process objects, process-environment,
pty/TTY, networking/TLS, process filters/sentinels, process groups, exec-path
search policy, complete coding systems or the owner event pump. Those remain
separate integration requirements. It never claims full mu4e/Doom readiness.

## Evidence

Synthetic Python children exercise raw stdout/stderr bytes, stdin EOF, nonzero
exit, argument-vector literal preservation, last-residence cancellation/reaping,
and output-limit cancellation/EOF signaling. An ignored, explicitly configured
integration test runs the actual official matched mu 1.12.9 backend against
only the generated three-message fixture. It sends ping/find/view(mark-as-read
nil)/quit, drains both streams, and validates FE + hexadecimal **byte** length +
FF framing, decoded Unicode headers and actual view source-path response.
No real mail, account, attachment extraction, synchronization or sending occurs.
