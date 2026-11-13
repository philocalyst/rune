# Opt-in GNU batch oracle

Set `RUNE_ELPROP_BACKEND=batch` before running the existing `assert_elprop`
tests. GNU Emacs must be available as `emacs` on PATH. This invokes the official
`emacs -Q --batch --eval` interface; it does not start or connect to a daemon,
open a server socket, or require socket permissions. The default remains the
existing daemon backend; `RUNE_ELPROP_BACKEND=daemon` selects it explicitly.
Unknown values fail rather than silently selecting a backend. Selection is fixed
for each test thread after its first start; changing the setting afterward fails.

Each observation uses a fresh subprocess. This preserves property-test shrinking
and isolation, at a cost of startup time. The input is a Lisp-string-quoted single
form, never shell syntax. Trailing whitespace/comments are allowed; a second form,
garbage, or incomplete trailing syntax is rejected before evaluation. Result values use an exact per-request frame. Missing,
duplicate, malformed or contaminated frames, nonzero status, Lisp conditions,
invalid UTF-8, timeout and output exhaustion fail the oracle call, not the value
comparison. Incidental Lisp `princ` output is redirected while evaluating the form.

Limits: 64 KiB input form; 1 MiB combined captured stdout/stderr; 30-second wall
clock deadline per observation. Reader threads never buffer beyond the shared
output cap. Lisp quoting can expand a permitted input beyond an OS argument-size
limit; a resulting spawn error is reported, not treated as an observation. Unix timeout/output termination kills the dedicated subprocess group;
other platforms terminate the direct oracle child. Descendant containment on
non-Unix systems is not guaranteed. These are test-observation limits, not a
sandbox for executing untrusted Lisp.

Suggested enabled oracle checks: `library::elprop::tests::test_eval`,
`alloc::test::test_make_vector`, and the two `fns` base64 property tests.
`fileio`'s ignored tilde-expansion test remains a separate unsupported behavior.

The ignored `gnu_single_form_reader_accepts_comments_rejects_extra_or_incomplete_forms`
regression can be run explicitly with GNU Emacs on PATH. It covers comments,
multiple forms, garbage, and incomplete trailing reader syntax.
