# Owner process event delivery scope

`accept-process-output` and the native owner-turn polling hook deliver real pipe
transport events on the Context owner thread. Worker threads own bytes and native
status only. Each callback is rooted, freshly fetched, and invoked after registry
borrows are released. EOF/exit and sentinel-attempt state are recorded before
callbacks, so recursive polling or signals do not replay consumed events.

Binary stdout reaches custom filters as an exact ByteString. UTF8 text mode keeps
partial codepoints across chunks. Invalid/incomplete output clears decoder carry,
retires further delivery and terminates the child, then signals an error rather
than substituting characters. No sentinel callback is invoked after decoder failure. The separate stderr pipe is currently bounded ASCII diagnostic
output with every control/non-ASCII byte escaped, not part of the stdout RPC filter. Both EOFs and native exit are required
before sentinel dispatch. Successful normal exit uses `finished\n`; named signal
messages are currently numeric descriptions, not GNU's full signal-name table.

This is a bounded pipe subset, not the complete GNU process event loop. Individual
waits are limited to ten seconds; GNU's indefinite default wait is capped at ten
seconds here. A zero/negative wait polls without waiting. At most 1024 events are
consumed per entry; budget errors occur before dequeuing another event. Timers,
ptys, sockets, arbitrary UI work and idle wake transport are separate work. Native
callers must schedule another poll if events remain after an error or budget.

Completed process records retain rooted metadata until Context teardown so
retained process objects keep working. Active lists/name lookup omit records
after sentinel dispatch is committed. This intentionally has a strong-retention
resource limit until a complete weak registry lifecycle is implemented.

The event/time budgets do not preempt a running Lisp/native callback. Lisp QUIT
safe points remain active, but a blocking native operation may delay interruption.
There is no whole-callback wall-clock deadline or general native preemption claim.
