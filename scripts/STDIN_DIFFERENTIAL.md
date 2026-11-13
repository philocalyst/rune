# Supported-stdin differential bridge (INF-03 initial slice)

This alternative to the blocked `run_differential.py` prototype uses Rune's
existing `--eval-stdin` CLI. Direct source forms return marker lists; the Rust
CLI prints those values. Rune need not expose Lisp `read`, `eval`, `princ` or
`prin1`. GNU's separate adapter uses its real `-Q --batch --load` interface and
available Lisp printers. Requires Python 3.9+ and already-installed runtimes.

```sh
python3 scripts/run_stdin_differential.py scripts/corpora/stdin-differential-smoke.json \
  --rune ./target/release/rune --oracle emacs --timeout 60 \
  --revision "$(git rev-parse HEAD)" --oracle-revision your-gnu-tag \
  --output /tmp/stdin-differential.json
```

**Validation boundary:** a real Rune `8e1ac02` smoke ran the three-case corpus:
arithmetic yielded `(value (3 -1 42))`, Unicode indexing yielded
`(value (2 955))`, and `(car 1)` yielded an error envelope, with complete protocol
and exit 0. This establishes that tiny Rune bridge fixture, not GNU agreement. GNU was not
available locally; GNU-adapter tests use fake processes and are not oracle
integration evidence. Do not claim a measured cross-runtime pass rate until the
actual GNU executable has run the same corpus.

Run fixtures without either runtime, or opt into the small existing-Rune check:

```sh
python3 -m unittest discover -s scripts/tests -v
RUNE_TEST_BINARY=/path/to/already-built/rune python3 -m unittest discover \
  -s scripts/tests -p test_stdin_differential.py -v
```

## Input and environment

The seed file is a JSON array of 1–5000 objects with unique nonempty `id` and
`form` strings only. Form bodies may contain multiple expressions, evaluated by
`progn`. Every body must be one physical line, with no CR, LF, NUL or semicolon
anywhere. This conservative restriction also rejects semicolons inside strings
or character literals; it avoids parsing/comment-rewriting claims. Use Lisp
backslash escape spellings inside string source when appropriate. Inputs are
trusted executable code and must be syntactically valid; this is not a sandbox.
The smoke corpus is three probes, not the roadmap's reproduction catalogue.

Rune stdin bypasses normal bootstrap. GNU `-Q` has GNU's startup environment.
These environments are intentionally not asserted equivalent. Functions needing
preloaded Lisp may fail on Rune. The batch shares state between cases, runs in
the caller's working directory, and has no state snapshot or isolation. A temp
file-backed Python transport feeds Rune stdin without a blocking input pipe.
`--rune-arg=VALUE` and `--oracle-arg=VALUE` add options; revision strings are
caller-provided labels. Flags must not alter the expected bridge/identity.

## Observations and honest comparison

Both adapters return `(value RESULT)` or `(error CONDITION DATA)` envelopes.
Framed observations must arrive in order, uniquely, with a final completion and
zero runtime exit. Records observed before ordinary timeout/incomplete exits are
retained, but such execution remains unsuccessful. Protocol-corrupt observations
are discarded. A missing oracle produces an error report, never evidence of a
matching run. Exit status is nonzero for incomplete runtimes, missing observations
or mismatches. Matching error envelopes count as matches, not successful values.

Comparison is **exact printer text**, not structural equivalence. GNU print
settings prevent some avoidable formatting differences, but Rune does not honor
those print flags. Multiline/noisy/circular results can fail transport or time
out. Printer spelling, object addresses, hash iteration order and float rendering
can cause honest mismatches. There is no normalizer, accepted-difference registry,
state comparison, generator, shrinking, persistent per-job server or throughput
claim. This is not complete INF-03 or its million-forms-per-minute gate.

Each runtime has its own finite positive timeout and 4 MiB output cap; sequential
execution can approach twice the timeout plus bounded cleanup. Process capture
inherits the ERT runner's POSIX group cleanup and non-POSIX descendant caveats.
Reports include raw output, so treat them as potentially private source/runtime
data. Results are written locally only, not automatically published.
