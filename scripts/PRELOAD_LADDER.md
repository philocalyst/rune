# Explicit preload ladder (INF-05 initial measurement slice)

Measure sequential loading of a caller-supplied Lisp manifest in one fresh Rune
process. Requires Python 3.9+ and an already-built Rune. It does not require a GNU
executable. The fixture suite uses fake processes and alone does not establish
real runtime integration.

Prepare a JSON manifest such as `["emacs-lisp/byte-run.el", "subr.el"]`, in the
exact order you intend to measure, then run:

```sh
python3 scripts/preload_ladder.py /path/to/manifest.json \
  --lisp-root /path/to/emacs/lisp --binary ./target/release/rune \
  --revision "$(git rev-parse HEAD)" --source-revision your-emacs-tag \
  --timeout 60 --output /tmp/preload-ladder.json
```

There is no inferred or hard-coded claim that this example is GNU's real preload
order. The command does not parse/evaluate GNU `loadup.el` conditionals, establish
its platform configuration or emulate its setup. Source/manifest provenance and
revision labels are caller supplied. Use the same manifest/environment when
comparing revisions. The report identifies normalised root-relative file names.

Manifest entries must be 1–5000 unique relative `.el` paths within an existing
source root; path traversal and symlinks resolving outside it are rejected.
Missing declared files are allowed to become an actual measured load blocker.
This path check is input hygiene, not a sandbox: Lisp is trusted executable code
and may access other files or change load-path itself. It is the caller's job to
supply unpatched sources if that is the desired measurement.

## Runtime bridge and results

The runner uses Rune's existing `--eval-stdin` path, which deliberately bypasses
its normal bootstrap. The initial `load-path` contains only the supplied source
root, not Rune's bundled patched Lisp. This is a raw-core explicit ladder, not a
GNU-equivalent startup environment; missing preparatory setup can be the first
blocker. Files run in order with shared state, stopping load attempts at the
first caught error. Subsequent rows become `not-attempted`.

Direct forms return simple marker lists; the Rust CLI prints those values. This
avoids depending on unavailable Lisp `read`, `eval`, `princ` or `prin1` primitives.
It uses exposed `load`, `list`, `setq`, `if`, `progn` and `condition-case`. A small
Python transport opens a temporary file as runtime stdin; this avoids feeding a
large manifest through a blocking pipe and stays in the capture process group.
Extra runtime options may use `--arg=VALUE`, but must not alter the expected CLI
bridge or interpreter identity. Ordinary `--load` arguments do not bootstrap
`--eval-stdin` because Rune handles that mode first.

`loaded_prefix` counts only consecutive observed successful loads. `first_blocker`
identifies a caught load error, without serializing its full condition data.
Timeout/exit/protocol failures are explicit; unobserved files are `not-observed`,
not fabricated blockers. Partial observations survive ordinary incomplete runs;
protocol-corrupt records are discarded. Every expected marker and final completion
must be present in order with a zero process exit for runtime status `completed`.
That status alone is not ladder success: any blocker makes the command exit
nonzero. A completed load proves nothing about behaviour of functions it defines.

The whole process is bounded by a finite positive timeout and a 4 MiB output cap.
It shares the ERT capture's POSIX group cleanup and non-POSIX descendant caveats.
A missing runtime may appear as an unsuccessful transport/runtime exit with its
launch traceback retained, never a successful empty ladder. Reports are local
JSON only, not published dashboard or M0 gate evidence. No semantic correctness,
full GNU preload percentage or corpus coverage claim follows from a small manifest.

## Bounded integration check

A local smoke check against the already-built Rune revision `8e1ac02` loaded two
generated `.el` files in order, with the second reading state set by the first:
exit 0, `loaded_prefix: 2`. A manifest with a missing middle file reported exit 1,
prefix 1, that file as blocker and the final file as not attempted. This verifies
the CLI bridge for that tiny generated fixture, not GNU's preload ladder. The
check exposed two important bridge bounds now covered by fixtures: every complete
form must occupy one physical input line, and current Rune `load` accepts three
arguments, so the driver uses `(load PATH nil t)` rather than GNU's optional fourth
argument. No GNU source loading or semantic suite result was established.
