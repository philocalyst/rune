# Declared primitive census (INF-05 initial slice)

This Python 3.9+ scanner compares GNU C `DEFUN` declarations with Rune Rust
`#[defun]` declarations by Lisp name. It measures **declared name coverage**,
not implemented functionality, runtime registration, correctness or passing tests.
No GNU executable or Rune build is required.

Run from the repository root with explicit checkout identities:

```sh
python3 scripts/census.py --gnu-root /path/to/emacs --rune-root . \
  --gnu-revision "$(git -C /path/to/emacs rev-parse HEAD)" \
  --rune-revision "$(git rev-parse HEAD)" --output /tmp/census.json
python3 -m unittest discover -s scripts/tests -v
```

Both roots must contain a nonempty `src` directory of the expected language.
GNU scanning recursively visits `src/**/*.c`; Rune scanning recursively visits
`src/**/*.rs` and `crates/**/*.rs` if present. Missing inputs, unsupported primitive
name literals, malformed recognised declarations and empty declaration sets fail
explicitly rather than producing a fabricated zero baseline. Revision strings
are caller-supplied labels, not independently verified checkout identities.

JSON records unique declared GNU names, their intersection with Rune, sorted
missing/present names per GNU C file and globally, Rune-only names, all declaration
locations and duplicate-name diagnostics. Duplicates count once globally; a GNU
name declared in two files contributes to both per-file sets. Output ordering is
stable for the same source trees and revision labels. Dirty working-tree edits
are measured even if a revision label names a clean commit.

## Deliberate bounds

The scanner is lexical, not a C preprocessor or Rust parser. It recognises direct
`DEFUN("name", ...)`, direct `#[defun]` with underscore-to-hyphen default names,
and `#[defun(name = "name", ...)]` overrides, including multiline declarations.
It ignores comments and quoted/raw-string contents. Names must use ordinary
unescaped ASCII string literals. It does not expand macros, evaluate conditional
compilation, follow `include!`, discover runtime aliases or model link-time
registration. Macro-generated declarations and alternative attribute spellings
are outside this initial slice. Source declarations inside inactive conditional
branches or macro templates may be counted. Neither presence nor absence of a
name proves runtime availability. Stub/no-op declarations are present names.

This is not the complete INF-05 package: C-variable coverage, preload ladder,
package survey, CI publication and dashboard integration remain future work.
