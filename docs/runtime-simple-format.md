# GNU-style format conversions: qualified width/precision

The native `format` primitive, and its existing `message`/`format-message`
callers, share a pure helper in `src/format.rs`. The routing module is local to
`editfns`, avoiding changes to executable/native-application modules.

Supported conversions are `%d`, `%o`, `%x`, `%X`, `%f`, `%e`, `%g`, `%c`, `%s`,
`%S` and `%%`. Common `-`, `+`, space, `#` and `0` flags, decimal minimum field
width and decimal precision are implemented. Numeric inputs include fixnums,
bignums and floats. Integer conversions truncate finite floats and preserve a
minus sign instead of rendering host-width two's-complement bits. Integer
precision supplies leading zeros and disables width's zero flag; alternate
forms supply octal/hexadecimal prefixes. Zero-valued integer precision zero,
float-to-decimal zero, and nonfinite decimal precision retain GNU's distinct
behavior, qualified by explicit tests.

Floating precision counts places (`f`/`e`) or significant digits (`g`), defaulting
to six. Precision zero for `g` means one significant digit. Alternate form retains
a point/trailing fractional zeros. Exponents have a sign and at least two digits.
Unlike numeric literal printing, `%g` does not append `.0` to integral output.
Nonfinite decimal/floating conversions use `inf`/`nan`; octal/hex conversions
reject nonfinite floats. Precision beyond the separate literal-printer limit of
15 is supported and tested through 40 digits.

String width and precision use default display columns, not UTF-8 bytes. They
handle common CJK/emoji width, combining/zero-width characters and GNU default
control widths (tab 8, newline 0, other C0/DEL 2, C1 4). Precision truncates only
at UTF-8 character boundaries and width pads without truncation. `%c` ignores
precision. The official registry dependency `unicode-width` is pinned to 0.2.2;
custom display tables, composition/font-dependent widths and nondefault tab
width are not modeled. The Unicode table version can differ from GNU's.

`%s` copies direct string contents without added escapes. `%S` uses existing
object display and quotes direct strings. Backslash is an ordinary literal,
not a percent escape. Extra arguments are evaluated but ignored by formatting.
Malformed conversions, unsupported fields and dimension overflow return errors;
an unsupported non-ASCII conversion cannot split UTF-8 or panic.

No user string is executed as a host formatting program. Dimensions use checked
arithmetic and the supported signed-C-int domain. Padding/output reservations
are fallible where controlled by the helper. Large valid fields, strings and
bignums can still consume large resources; this is not an allocation or
interruptibility budget, nor a claim of fully fallible allocation.

## Explicit limitations

Positive positional argument fields such as `%2$s` are supported, including GNU's mixed positional/sequential cursor behavior. Zero positional indexes remain unsupported. Dynamic `*` fields are invalid in GNU and are rejected. Marker arguments,
text-property preservation, invalid-UTF-8 byte strings, exact unibyte result
classification and GNU shared-empty-string identity optimizations are not
implemented. `%c` accepts Unicode scalars; GNU's extended non-Unicode character
space remains outside the UTF-8 model.

Top-level `%S` UTF-8 strings escape quote and backslash, with default literal
newline/control-character behavior. Nondefault print escaping controls remain
unsupported. Byte-string printing is unchanged outside the nested traversal.

Top-level `%S` symbols escape reader delimiters and ambiguous numeric/leading
dot/question-mark names; empty names print as `##`. Numeric recognition reuses
the reader grammar. Nondefault print-gensym and obarray context remain unsupported.

Lists and vectors use a separate iterative traversal. `%s` leaves nested strings
and symbols unquoted, while `%S` uses the corrected readable leaf escaping.
Proper/improper lists, vectors, repeated acyclic sharing, quote/function
abbreviations and backquote-context comma abbreviations are qualified against
GNU. No Lisp allocation or evaluation occurs while borrowed values are visited.

This traversal has a 100,000-task work/stack limit and a 16 MiB output limit,
with checked/fallible helper-controlled reservations. These are local safeguards,
not an outer-format allocation or interruptibility guarantee. Numeric/scalar leaf printing, readable-symbol helpers and legacy opaque-object
display can allocate temporary strings before the final append-size check. Thus
the output/task limits are not a universal peak-memory or OOM budget.
Circular lists/vectors return an explicit unsupported error; GNU with
`print-circle` nil instead emits depth references such as `(1 . #0)` and `[#0]`.
Both sides of this intentional divergence are tested. Nested records, hash
tables, bytecode functions and byte strings are explicitly unsupported rather
than sent back through the recursive legacy printer. Top-level opaque objects
retain their existing display behavior.

General printer context, including custom `float-output-format` for `%s`/`%S`,
print-level/length, print-circle labels and escaping controls, is not threaded
through. Existing format-message quote-style limitations and incomplete named
GNU error conditions also remain.

Qualification adds 1,190 GNU numeric flag/width/precision cases, 162 default
Unicode/control-column cases and 144 high-floating-precision cases to the
previous 105 simple numeric cases and focused regressions. Aggregate tests
explicitly select default printer settings, comparing content rather than
shared-string labels or transport escape policy. The reference is GNU Emacs
30.1's official Formatting Strings manual and live batch oracle.
