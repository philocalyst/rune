# Opt-in native editor

The root binary has an opt-in `native` feature and `--native` mode. Default
terminal/eval behavior is unchanged. The native path constructs one Context,
one persistent rooted Env and their shared editor gateway on its dedicated owner
thread. GPUI receives only immutable prepared frames, bounded plain-text context,
ordered owned input and explicit outcomes.

With the combined root lock used for verification:

```sh
cargo check --offline --locked --features native
cargo run --locked --features native -- --native \
  --native-eval '(setq greeting "λ🙂")' \
  --native-eval '(insert greeting)'
```

The second command is a display-executor procedure, not a claim it has been run.
Linking and a supported desktop/display are additional prerequisites. Do not
bypass socket or sandbox restrictions to launch it.

Startup sources are ordered evaluator events, capped at 64 sources and 65536
bytes per source. Variables persist between sources. An evaluation may edit and
then signal; the partial edit is published once, the error is reported separately,
and frame failure never retries evaluation. Native typing, scoped IME commits,
explicit commands and evaluation share one owner FIFO. Initial context may be
temporarily disabled while startup evaluation updates it.

This first native CLI uses initialized primitive symbols/variables but does not
load the full Lisp bootstrap. `--load`, `--repl`, `--eval-stdin`, `--dump` and
`--load-dump` conflict with `--native`; `--native-eval` requires it. This avoids
silently loading a different Context or evaluator authority. The feature must be
explicitly enabled at build time.

The editor currently uses fixed native movement/edit commands and platform-only
printable text. Typed C-x keymaps, shift selection, a live evaluation prompt,
clipboard shortcut UX and full complex shaping are not provided by this path.
C-g cancels local preedit and requests QUIT for a currently active evaluation.
The UI and owner share a plain request-scoped channel; a fresh atomic token is
installed only during each callback and removed on every return path. An idle
C-g is not carried into a future evaluation, and a delayed old token cannot
cancel the next request. C-g before a queued evaluation becomes active is not a
request to cancel that future evaluation.

The interpreter polls at form boundaries and the bytecode VM at instruction
boundaries. `inhibit-quit` preserves a pending request; cleanup runs through the
existing nonlocal-exit machinery without repeatedly polling the same request.
A program may catch QUIT or inhibit it. Reader work, long native subroutines,
allocation, blocking locks and nonterminating cleanup are not preemptible. No
owner thread is forcibly killed and no evaluation is retried after interruption.
See [runtime quit safe points](runtime-quit-safe-points.md).

A linked, headless native owner-pipeline test verifies pending/inhibited QUIT,
cleanup and partial edits, publication failure without replay, and the next FIFO
request with a fresh token. Dependency-light tests verify the busy-input C-g
signal path. These are not OS keyboard callback or native window tests.

Evaluation/input failures are currently reported to stderr, not a finished
in-window recovery panel. A failed initialization preserves queued events for
reporting; no successful composition acknowledgment is fabricated.

Compiler/tests are separate from native behavior. Candidate placement,
compose/cancel, focus, resize, close and real font/shaping behavior still require
a supported native display test. No window or OS IME verification is implied by
`cargo check`.
