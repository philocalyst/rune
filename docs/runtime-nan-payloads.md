# GNU quiet-NaN literal payloads

The reader, `string-to-number` and default float printer preserve the sign and
51-bit payload of IEEE binary64 quiet NaNs. GNU's decimal spelling is
`PAYLOAD.0e+NaN`, with an optional leading minus sign. For example,
`1.5e+NaN` reads with payload 1, and prints as `1.0e+NaN`: fractional mantissa
digits do not contribute to the payload.

The leading unsigned decimal integer is reduced modulo 2^51. Parsing uses
wrapping integer operations without allocating a BigInt. Overflow of the
intermediate machine integer does not change that modulo result. GNU's unusual
dot-leading syntax is retained: `.5e+NaN` gets payload 2^51-2 because the initial
reader digit conversion of a dot is -2. Numeric syntax is validated by the caller
before the shared payload helper runs.

The implementation follows GNU Emacs 30.1
[lread.c](https://github.com/emacs-mirror/emacs/blob/emacs-30.1/src/lread.c)
and [print.c](https://github.com/emacs-mirror/emacs/blob/emacs-30.1/src/print.c).
A GNU batch oracle qualifies 2,064 signed input cases through both readers,
including 512-bit decimal prefixes, overflow boundaries and fractional syntax.
Rune tests exercise 8,192 signed quiet-NaN bit-pattern roundtrips and 512 large
prefix cases, plus Lisp `eql` distinctions between payloads and signs.

## Structural versus numeric comparison

Lisp `equal` and `eql` compare float representation bits. Equal quiet-NaN
payload/sign pairs compare equal structurally, while different payloads/signs
and positive versus negative zero are distinct, including inside lists/vectors.
Numeric `=` and ordered comparisons remain IEEE: NaNs are unordered and never
numerically equal, while both signed zeros compare numerically equal.

The generic object hash is compatible: equal float bits produce equal hashes.
Its existing signed-zero normalization may give two unequal zeros the same hash,
which is a permitted collision, not a violation of the equality/hash contract.
This does not complete the separate Lisp hash-table `eq`/`eql`/`equal` test modes.

## Deliberate limits

These literals always construct a **quiet** NaN. They do not encode the signaling
bit. Printing a signaling NaN preserves its sign/payload text, but reading that
text quiets it. This is not an all-64-bit-pattern signaling-NaN roundtrip claim.
NaN results from arithmetic and platform math may be canonicalized or propagated
differently by the target/compiler; this change does not promise identical
payload selection for every arithmetic operation. Custom `float-output-format`
bindings are supported by `number-to-string` for finite values only; general
object printing still uses the default format. GNU nonfinite spellings bypass
custom format policy, retaining NaN payload/sign.
