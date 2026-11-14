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
printable text. Typed C-x keymaps, shift selection and full complex shaping are
not provided by the document renderer. The evaluator field below has its own
bounded clipboard/select-all editing and command submission.

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

Evaluation/input failures remain on stderr and are surfaced in the bounded
palette status/history after window construction. This is not a durable recovery
journal; failures before the first frame still use stderr. A failed initialization preserves queued events for
reporting; no successful composition acknowledgment is fabricated.

Compiler/tests are separate from native behavior. Candidate placement,
compose/cancel, focus, resize, close and real font/shaping behavior still require
a supported native display test. No window or OS IME verification is implied by
`cargo check`.


## Evaluator command palette

Ctrl+Enter (Command+Enter where applicable) or the **Evaluate Lisp** overlay
button opens a temporary UI-only source field without resizing the document.
Enter admits one request to the same owner FIFO; held-key repetition and another
pending manual request cannot submit it again. Shift+Enter adds a source newline
(displayed as whitespace in the single-line field). Escape cancels preedit first;
a second Escape closes the field and returns document focus.

Printable text uses the platform text callbacks only. UTF-16 ranges reject
surrogate interiors for mutation; readonly queries report adjusted exact ranges.
Preedit never evaluates; confirm it and release the confirmation key before a
fresh Enter evaluates. A key-up fence prevents a final IME callback followed by
the same Enter key from submitting the source. Some IME confirmation paths may
require an extra release/Enter cycle; real platform behavior remains unverified. Blur, deactivation and bounds
changes cancel UI preedit and invalidate cached geometry. Text/candidate callbacks
from an unfocused field reject rather than mutate a hidden draft. These paths
are source-wired and compiler-checked, not OS callback verification.

The field supports select-all, copy/cut/paste, scalar cursor movement, Home/End,
and deletion. Alt+Up recalls the last admitted manual source, but never replays
it. Shift-selection and multi-line visual editing are not implemented. Drafts,
preedit and recalled source are limited to 65536 bytes. Request IDs are reserved
on every attempt, including rejection, so a late rejection cannot complete a
later retry. Definite rejection retains the draft; no outcome triggers resend.

A separate foreground-woken report mailbox shows correlated completion/error
status, input/publication notices on a separate line, and a bounded 8-outcome
history (latest 3 visible). Startup sources and one manual attempt bound pending
outcomes to 65; rejected attempts remain gated until their callback is consumed.
Error display payloads are truncated to 240 characters. Wakes occur after report
locks drop and do not keep the document owner's publication channel alive.

This is a command runner, not a value-printing REPL: successful evaluation reports
completion, while buffer changes are visible in the editor. Arbitrary Lisp return
objects are not passed to GPUI. OS keyboard/IME/focus behavior, candidate placement
and visual appearance still require a supported native window test.
