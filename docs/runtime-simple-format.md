# Simple GNU-style format conversions

The native `format` primitive, and its existing `message`/`format-message`
callers, share a pure helper in `src/format.rs`. The routing module is local to
`editfns`, avoiding changes to unrelated executable/native-application modules.

Supported modifier-free conversions are `%d`, `%o`, `%x`, `%X`, `%f`, `%e`, `%g`,
`%c`, `%s`, `%S` and `%%`. Numeric arguments support fixnums, bignums and floats.
Integer conversions truncate finite floats, and radix conversions preserve a
minus sign instead of printing a host-width two's-complement bit pattern.
Floating conversions use six decimal places (`f`/`e`) or six significant digits
(`g`), with GNU exponent spelling. Unlike numeric literal printing, `%g` does
not append `.0` to integral-looking output. Nonfinite `d`/`f`/`e`/`g` conversions
use `inf`/`nan`; octal/hexadecimal conversions reject nonfinite floats.

`%s` copies string contents without adding escapes; `%S` uses existing Lisp
object display and quotes strings. Backslash is an ordinary literal character,
not a percent escape. Extra arguments are evaluated by the evaluator but ignored
by the formatter, matching GNU. Missing arguments and incomplete/unsupported
conversions return errors. Unicode traversal is by characters, so an unsupported
non-ASCII conversion does not split UTF-8 or panic.

No user format string is executed as a host formatting program. The helper uses
constant Rust format operations, checked character conversion and the shared
finite-float formatter. It adds no unsafe code, FFI or external-process dependency.
Large preexisting strings/bignums may still allocate large output; this is not an
allocation/interruptibility budget or a fallible-allocation completion claim.

## Explicit limitations

Flags, field width, precision and positional arguments are not implemented, and
are rejected explicitly rather than partially rendered. Marker arguments,
text-property preservation, raw invalid-UTF-8 byte strings and exact unibyte
result classification are not implemented. `%c` accepts Unicode scalar values;
GNU's extended non-Unicode character space remains outside the UTF-8 model.
General printer context, including custom `float-output-format` for `%s`/`%S`,
`print-length`, `print-level` and related controls, is not threaded through this
helper. Existing `format-message` quote-style limitations remain unchanged.
Nested `%s` values use the existing readable object display rather than GNU's
`princ` behavior, and `%S` inherits the existing symbol-escaping limitations.
Named GNU error conditions and exact diagnostics are not claimed.

Qualification includes a 105-case aggregate GNU comparison spanning seven
numeric conversions and signed integer/float/bignum/nonfinite extremes, plus
focused object/string/Unicode/percent/extra-argument and rejection regressions.
