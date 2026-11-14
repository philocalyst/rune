# Default loader error excerpts

Normal loader failures show the source byte range and a bounded, ASCII-escaped
call-head identifier or static type label. String literals, comments, arguments
and arbitrary object graphs are not echoed from the failed form. This avoids
large source dumps and accidental literal-value disclosure in default logs.
The identifier limit is 48 characters; control and non-ASCII characters escape.

Explicit Rune verbose debugging (`enable-debug`) retains the original full-source
read/error output. Tests pass that choice explicitly and do not toggle shared
debug state. `init-file-debug` is a separate GNU startup state variable, not a
request to print raw source in Rune's verbose diagnostics.

This is not a general secret-redaction system: a program can intentionally put
sensitive text in a function identifier or signal message. Structured EvalError
summaries are a separate bounded diagnostic layer.
