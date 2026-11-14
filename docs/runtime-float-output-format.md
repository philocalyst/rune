# Scoped float-output-format support

`number-to-string` honors the dynamically bound `float-output-format` variable
for finite floats. Integer conversion remains decimal. Nonfinite values retain
GNU infinity/quiet-NaN spellings regardless of the selected format.

GNU's validator permits `%.Ne`, `%.Nf`, and `%.Ng`, where N is a decimal
precision from 0 through 15; explicit zero is permitted only for `f`. Missing
precision (`%.e`, `%.f`, `%.g`) and leading-zero precision fields follow GNU's
rules. Invalid strings, non-string values and nil fall back to the default
roundtripping float printer. GNU's C-string validation ignores content after
the first embedded NUL, which this scoped validator also does.

Fixed/scientific precision counts digits after the point; general precision
counts significant digits, strips trailing coefficient zeros, and uses GNU's
notation threshold and signed, at-least-two-digit exponents. `%.0f` deliberately
prints an integral-looking string with no `.0`; `%.f` adds `.0`. Thus custom
formatting is not a promise of numeric-kind or exact-bit read-back.

No policy string is executed as a host formatting program. A validated kind and
bounded precision select Rust formatting operations; output for supported
binary64 values is bounded to a few hundred characters. This policy boundary
has no unsafe code, FFI, global mutable state, or external process dependency.

The reference is GNU Emacs 30.1
[print.c](https://github.com/emacs-mirror/emacs/blob/emacs-30.1/src/print.c).
A standalone driver extracted from the exact formatter source matched GNU in
14,580 cases across 54 policies and 270 float bit patterns/boundary values.
Rune unit tests cover precision, notation, validation, embedded NUL, nonfinite
bypass, and the native primitive's dynamic policy binding.

This is scoped support, not completion of the recursive object-printer policy.
Generic Object Display, error diagnostics and nested printed structures still
use the default formatter. The primitive and core helper are ready for future
explicit printer-context plumbing without consulting a process-global policy.
