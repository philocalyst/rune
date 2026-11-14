# Scoped native quit requests

This increment supplies evaluator machinery; native UI hookup is a separate
integration. No claim of live native interruption follows from these tests.

A rooted Env accepts an owner-local scoped `install_quit_token(Arc<AtomicBool>)`
guard. The guard owns only native state and retains no Env/Context borrow. It
restores the previous token on normal return or panic. Guards must be nested and
dropped in lexical order. Only the plain atomic is shared with input producers.

Polls occur at interpreter form entry and each VM instruction boundary. A
non-NIL dynamic `inhibit-quit` preserves the pending atomic rather than consuming
it. Otherwise one poll consumes the token and raises the structured `(quit)`
condition through existing rooted PendingExit machinery. ERROR handlers and
catch tags do not intercept quit; a quit condition handler can handle it. This
adds quit discrimination, not a complete condition inheritance hierarchy.

Interpreter unwind-protect cleanup inhibits token consumption using an owning
native depth guard, including nested cleanup and panic unwinding. Explicit Lisp
signals from cleanup are not suppressed and may supersede the pending exit, as
other cleanup errors already do. Saved excursions and dynamic/VM frames continue
to restore through their existing nonlocal-exit paths.

The transport must mint a fresh token for each active evaluation and stop
exposing it when that evaluation exits. Reusing one forever flag or clearing it
on request entry can respectively leak stale C-g into the next request or lose
a request. Idle C-g is a transport policy, not synthesized by Env. The native
adapter must install the scope around all evaluator-entry failure paths.

These polls do not interrupt long-running native subroutines, reader work,
allocation, host locks or individual arithmetic operations. They do not impose
a deadline, stack budget or instruction budget. Code may catch quit, inhibit it,
or perform nonterminating cleanup. Do not terminate the owner thread to enforce
cancellation; that would bypass root and editor cleanup.
