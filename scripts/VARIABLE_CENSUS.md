# Declared C-variable census (INF-05 initial slice)

Compare GNU `DEFVAR_LISP`, `DEFVAR_INT`, `DEFVAR_BOOL`, `DEFVAR_PER_BUFFER`,
`DEFVAR_KBOARD` and `DEFVAR_LISP_NOPRO` with Rune `defvar!` and `defvar_bool!`.
This is declared-name overlap, **not semantic coverage**. No executable, GNU
Emacs installation or Rust build is needed. Requires Python 3.9+.

```sh
python3 scripts/variable_census.py --gnu-root /path/to/emacs --rune-root . \
  --gnu-revision "$(git -C /path/to/emacs rev-parse HEAD)" \
  --rune-revision "$(git rev-parse HEAD)" --output /tmp/variables.json
python3 -m unittest discover -s scripts/tests -v
```

GNU scanning includes `src/**/*.c` and `src/**/*.m`; Rune scanning includes
`src/**/*.rs` and `crates/**/*.rs`. Both require an existing `src` directory
and at least one recognised declaration. Source paths and caller-provided
revision labels use the same conventions as [the primitive census](CENSUS.md).
Reports contain per-file and global unique-name counts, missing/present names,
Rune-only names, duplicate diagnostics, declaration kind and source location.
Invalid recognised declarations fail before replacing an existing report.

Rune names follow `build.rs`: one argument derives a name; two arguments mean
identifier plus **value**, even when the value is a string; three arguments mean
identifier, explicit name and value. Default names lowercase ASCII identifiers
and replace underscores with hyphens; `KW_` becomes a colon prefix. Nested value
expressions are separated lexically, not evaluated or type-checked.

## Bounds

This reuses the primitive census lexer, not a C preprocessor or Rust parser.
Only the six listed GNU macros and direct parenthesised Rune macro calls are
recognised. Names must be unescaped ordinary ASCII literals. Comments and
string contents are ignored; inactive conditional branches and macro templates
can still contribute declarations. Macro expansion, runtime aliases, Lisp
`defvar`, dynamically registered variables and alternative spellings are not
measured. Missing files do not stand for zero coverage.

A matched name proves nothing about runtime registration, initial values,
forwarding, buffer/keyboard locality, mutability, special-variable behaviour or
correctness. The report does not claim Appendix E's preload/package usage bands
or the complete INF-05 deliverable. Genuine GNU coverage requires the owner's
specified GNU checkout; local fixture results are not a GNU baseline.
