# String-input read

`read` parses the first object using the existing reader; it does not validate
trailing input. Every String input starts at zero, including a String bound to
`standard-input`. No buffer point or persistent string cursor is changed.
Unicode strings and ASCII unibyte strings are accepted. Non-ASCII unibyte input,
terminal/buffer/marker/function streams are explicitly unsupported, rather than
being coerced or fabricated. This is not full GNU stream-reader compatibility.

EOF and syntax failures use named conditions; EOF includes the current
`load-file-name` when non-nil. Detailed syntax wording is not fully GNU-identical.
Input is limited to 1 MiB at this boundary. The shared parser additionally rejects
nesting beyond 128 recursive forms, including existing loader/read-from-string
paths. These are explicit resource policies, not GNU limits or a total allocation,
wall-clock, OOM, or all-reader-dispatch compatibility guarantee.
