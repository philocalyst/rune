# Isolated saved-probe replay

Replay one concrete case from a `primitive_probes.py` report in a fresh Rune
process. This closes the gap between a first-unobserved *candidate* in a failed
batch and an actual isolated execution. It does not regenerate random inputs,
replay preceding state or shrink the case. Python 3.9+ and an existing runtime
are required; the supported stdin adapter supplies bounded process capture.

```sh
python3 scripts/probe_replay.py /tmp/primitive-probes.json \
  --binary ./target/release/rune --revision runtime-being-tested \
  --timeout 10 --output /tmp/probe-replay.json
```

By default it selects the report's `first_unobserved_candidate`. Use
`--case-id=ID` to explicitly select another saved case, including from a completed
probe report. A missing candidate, ambiguous/duplicate ID, unsupported input
schema or invalid concrete form is an error, not a successful replay. Invalid
input fails before replacing existing output. The source report's command or
binary path is never executed: runtime selection is caller supplied.

The input must have schema version 1, measurement
`bounded-primitive-form-probes` and a concrete `replay_cases` list. Older reports
without that list are rejected; retaining original concrete source is preferable
to silently regenerating from a seed. Every saved case must satisfy the stdin
bridge's one-line/semicolon-free bounds. JSON duplicate keys are rejected.
These checks establish structure, not trustworthy provenance. Forms are executable
Lisp, so replay only reports you trust.

Results include selected source, original runtime status/revision, caller-supplied
replay revision, fresh raw output/observations and one of two deliberately narrow
assessments: `isolated-runtime-completed` or
`isolated-runtime-failure-observed`. The second does not prove the same cause as
the original failure. The first may return a value **or a condition**; neither
proves semantic correctness, and it does not show that a state-dependent original
failure was spurious. Revision labels are not independently verified. No semantic
oracle, shrinking, automatic bug attribution or fix verification is implied.

Exit 0 means a complete fresh runtime protocol, even for an ordinary condition;
exit 1 means isolated runtime execution was unsuccessful. Argument/input errors
exit 2. Runtime timeout/output/cleanup and multiline-printer limitations are the
same as documented in `STDIN_DIFFERENTIAL.md`. A missing binary is a runtime
failure, not proof that the saved Lisp crashed. No builds or installations occur.

## Bounded real check

The saved seed-42 case `00019-division`, `(/ -1073741824 0)`, was replayed against
existing Rune `8e1ac02`: exit 101 with `attempt to divide by zero`, and replay CLI
exit 1. The report retained that exact source and raw panic output. This verifies
the replay tool's tiny real fixture on that revision, not later fixes or any
hour-long no-abort gate.
