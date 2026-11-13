# Panic-syntax inventory (INF-01 measurement slice)

A repeatable, build-free source inventory of direct `todo!`, `unimplemented!`,
`panic!`, `unreachable!`, `.unwrap(...)` and `.expect(...)` syntax. Python 3.9+
only; no GNU executable, Rust toolchain or installed dependencies required.

```sh
python3 scripts/panic_inventory.py --rune-root . \
  --revision "$(git rev-parse HEAD)" --output /tmp/panic-inventory.json
python3 -m unittest discover -s scripts/tests -v
```

The report records counts by spelling, per-file counts, all source locations
(one-based Unicode-character line/column), source file count and a caller-supplied
revision label. It scans `src/**/*.rs` and `crates/**/*.rs` in sorted order,
including zero-site files. A missing/empty source tree is an error, not a clean
zero. A source tree with Rust files but no matched sites legitimately yields zero.
Malformed recognised string/comment syntax fails before replacing an existing
report. Dirty edits are included even if the revision label names a clean commit.

## Exact limitations

The reviewed census lexer ignores comments and ordinary/raw strings. It does not
parse or resolve Rust: qualified macro calls count, but so do same-named local
macros/methods, macro templates, test code and inactive `cfg` branches. Direct
method syntax requires a dot, the method name and an opening parenthesis;
method turbofish and raw-identifier alternative spellings are not measured.
It does not expand macros or inspect generated code, dependencies, `build.rs`,
assertion macros, indexing, overflow, allocator failures or arbitrary abort APIs.

This is not a Clippy lint count, production-only census, runtime panic count or
proof that a site is unsafe. A site can be safe or unreachable; zero matched sites
cannot establish M0's no-abort gate. Clippy, primitive fuzzing, panic-boundary
handling and actual runtime tests remain necessary. JSON from different revisions
can be retained for counts comparison; there is no automatic line-based regression
gate, because moving code would create misleading site churn.
