# Upstream ERT measurement (INF-04 initial slice)

Requires Python 3.9+ and an already built Rune. Supply test files from the
GNU Emacs revision being measured; Rune does not vendor the upstream test tree.
Run from the repository root so file identities remain stable across commits:

```sh
python3 scripts/run_upstream_ert.py /path/to/emacs/test/src/data-tests.el \
  --binary ./target/release/rune --timeout 60 \
  --revision "$(git rev-parse HEAD)" --output /tmp/ert-current.json
python3 -m unittest discover -s scripts/tests -v
```

Directories recursively discover `*-tests.el`. Each file gets a fresh process
and generated ERT listener. Rune's current CLI supports `--load`, not `--batch`;
the runner intentionally uses that supported interface. To verify the driver
against a GNU oracle, supply `--binary emacs --flavor emacs` (adds `-Q --batch`).
Extra interpreter arguments can be supplied with `--arg=VALUE`. The timeout
must be finite and positive.

JSON includes per-file counts, raw output, exit status, elapsed time and pass
rate (null for no observed tests). Outcomes are passed, assertion-failed,
`rune-unimplemented`, other error, and skipped. Expected failures remain actual
failures for coverage measurement. `comp-tests.el` and `lcms-tests.el` are
permanently skipped as specified by the roadmap. The JSON labels are `failed`,
`unimplemented`, and `errored` respectively.

Missing binaries, bootstrap/load failures, absent completion records, crashes
and timeouts are explicitly not successful test runs. Earlier test records are
retained when a later test times out. A zero exit code alone proves nothing.
A missing ERT dependency is reported as could-not-load, not a fabricated set of
failed tests. This initial runner does not yet map crates to milestone suites,
render dashboard pages, or claim that the current runtime supports upstream ERT.
The unit suite uses fake interpreter processes; it does not establish Lisp
integration correctness. Test files are executable code and should be trusted.
Output is capped at 4 MiB per file; exceeding it is an output-limit failure.
On POSIX, a new process group is killed after every run, including timeouts.
On other platforms only the direct interpreter can be killed; inherited output
handles are bounded by a cleanup wait and reported as cleanup-error. Detached
children that escape the POSIX group still require an external CI/container
timeout. A run with no result records is no-tests, never qualified success;
selected and completed test counts must agree. Malformed or duplicate protocol
records are errors. Aborted ERT runs are errors.

Use `--baseline previous.json` for the per-test regression list. Every formerly
passing test that fails, disappears, times out before producing its result, or
belongs to an omitted file is a regression. Keep the same suite/path identities
between reports. Exit status is nonzero for regressions, incomplete files, or
any observed failed/unimplemented/errored result. Reports are still written on
those outcomes, allowing CI to retain honest baseline evidence.

## Render a per-file table

Convert a saved schema-v1 ERT report to Markdown without running either
interpreter. An optional previous report adds formerly-passing regressions:

```sh
python3 scripts/ert_table.py /tmp/ert-current.json \
  --baseline /tmp/ert-previous.json --output /tmp/ert-table.md
```

The table recomputes counts and rates from observed test outcomes, ignoring
stored `counts`, `pass_rate` and `regressions` fields. Rates include all observed
outcomes in the denominator, including skipped tests; no records means a dash,
not 0% or 100%. Incomplete statuses remain explicit, and files omitted from the
current report but present in the baseline appear as `missing-current`.
`completed` means the runner finished, not that every test passed. Baseline
regressions mean formerly-passing tests now absent or nonpassing, exactly as in
the runner; retaining a passed result in a subsequently incomplete file is not
itself a test regression, though the file remains visibly incomplete.

File/test/revision text is escaped for Markdown tables; raw interpreter output
is not embedded. Input schemas, statuses and outcomes are checked, and duplicate
JSON keys/file identities are rejected. Invalid reports fail before replacing
existing output. This validates report structure, not provenance or whether an
interpreter genuinely ran. An empty file list is displayed honestly as zero
files, never evidence of a successful suite. Rendering is local only: no
publication, CI artifact upload or roadmap milestone coverage claim is implied.
