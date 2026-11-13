# Seeded primitive-form probes (INF-11 / M0 initial slice)

Generate a small reproducible set of direct Lisp forms and run them through the
supported Rune stdin bridge, without a GNU or semantic oracle. Expected Lisp
conditions are ordinary observations, not failures. A runtime timeout, incomplete
protocol, abnormal exit or malformed observation is a finding, not automatically
a proven panic. Python 3.9+ and an existing Rune binary are required.

```sh
python3 scripts/primitive_probes.py --binary ./target/release/rune \
  --seed 42 --count 200 --timeout 15 --revision "$(git rev-parse HEAD)" \
  --output /tmp/primitive-probes.json
python3 -m unittest discover -s scripts/tests -v
```

The nine fixed template families are arithmetic, division, car, cdr, nth, length,
aref, substring and make-vector. Inputs mix small typed values, ill-typed values,
negative/boundary indices, Unicode strings, proper/improper lists, vectors and
zero divisors. Arithmetic magnitudes are bounded to 2^30; vector lengths to 8
(or -1). This avoids intentionally generating giant allocations. It is not a
general typed-argument generator, all primitives' coverage or a resource sandbox.

`--count` accepts 1–5000, default 200. Each form is one physical semicolon-free
line, matching the stdin bridge's bounds. A seeded Python PRNG chooses forms;
the full concrete `replay_cases` list is retained, so cross-version PRNG changes
do not destroy reproduction evidence. IDs include the position and template family.
The report also retains per-case source, observation, runtime status and raw output.

Raw-core stdin bypasses normal bootstrap and shares state through the batch.
Missing primitives and ordinary argument errors can produce condition envelopes;
the runner does not prove that every selected primitive was entered. It does not
check value/condition correctness, classify expected conditions, or compare state.
Multiline/noisy/circular results inherit the stdin bridge's transport limitations;
such a protocol failure is not by itself a runtime panic. First unobserved case is
only a candidate, never automatic crash attribution or shrinking. A failed run is
reported nonzero even if preceding cases produced normal observations.

One process has the configured finite positive timeout and 4 MiB output cap.
Inherited cleanup and non-POSIX descendant caveats are documented in the stdin
bridge. No loop runs for an hour and no M0 no-abort gate is certified by a short
successful batch. No build, installation, publication or source modifications occur.

## Real bounded finding

Against existing Rune revision `8e1ac02`, seed 42/count 40 observed ten values and
nine conditions, then exited abnormally before case `00019-division` completed.
That case is `(/ -1073741824 0)`. A separate one-case rerun reproduced exit 101
with `attempt to divide by zero`, establishing a concrete panic for that revision,
not a claim about later changes. The runner preserved the remaining 21 rows as
not observed rather than passing or inventing conditions. This is a reproduced
finding, not semantic-oracle validation or evidence the whole probe corpus passed.
