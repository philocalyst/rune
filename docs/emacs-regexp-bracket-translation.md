# Emacs bracket translation

The translator parses a bracket expression before emitting Rust regex syntax.
Bare `[` and backslash inside Emacs classes are literal characters, not nested
classes or escape prefixes. Initial `]`, negation, ordinary ranges and literal
Rust set-operator punctuation are handled explicitly. Descending character ranges
contribute no characters, matching GNU. POSIX named tokens are kept separate from
literal characters, so the original derived.el pattern for backslash followed by
`[` or `{` does not become an invalid Rust nested class.

Raw-byte regex patterns use a private one-byte-per-scalar syntax IR and emit high
byte escapes only AFTER class parsing. This is not UTF8 decoding or a wire-data
conversion: byte order, high-byte literals/ranges and byte capture positions stay
native. Unicode matching retains character capture positions.

Limits: at most 1 MiB of pattern input, 16384 items in one bracket class and 128
bytes in a POSIX name. Malformed/unclosed or unsupported named-range classes fail
closed as regex build errors; translation stops at the rejection rather than
rescanning suffixes quadratically. These are scoped work/allocation controls, not
universal peak-memory/wall-clock guarantees. Existing backend compile limits also
apply.

This does not add complete syntax-table/Unicode POSIX class equivalence, full
Emacs regex escapes or error-message parity. The existing approximate `word`
class mapping is unchanged; named classes used as range endpoints are explicitly
unsupported, not interpreted as Rust class algebra. Tests cover the actual
original derived.el case, an independent GNU ASCII matrix, raw high bytes and
existing matching/capture/inhibition behavior.
