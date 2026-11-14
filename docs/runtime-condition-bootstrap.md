# Minimal builtin condition metadata

The central generated init_variables entrypoint installs Env-local condition
metadata through rooted setters. Main, worker runtimes and the persistent native
owner invoke this entrypoint. Raw Env::new alone does not initialize Lisp state.
The initializer skips any existing property, including explicit NIL, so repeated
startup initialization does not overwrite user condition metadata.

GNU30.1 defaults are installed for error, quit, wrong-number-of-arguments,
wrong-type-argument, void-variable, void-function, invalid-function,
setting-constant, arith-error, range-error and overflow-error. The Rune-specific
rune-unimplemented condition inherits error. Lists are flattened metadata, not
recursive superclass-property resolution. Each Env owns its own traced lists.

Fresh lists are allocated with shared Context access and published into rooted
Env properties before collection can run. The existing same-owner startup
contract applies; this does not introduce an arbitrary foreign Object adopter.
No raw Env constructor lifetime erasure or raw ObjectMap mutation is used.

This does not convert generic anyhow/native TypeError failures into precise Lisp
signals, initialize every GNU condition/message, implement debugger hooks, or
remove the minimal matcher fallback needed before Lisp initialization. Generic
malformed-list diagnostics and native-operation interruption limits remain.
