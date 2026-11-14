# Arithmetic integer-width qualification

The runtime's arithmetic result gate applies `max(integer-width, 128)` magnitude
bits to `+`, `-`, `*`, `/`, `1+` and `1-`, both at Lisp native boundaries and the
corresponding VM arithmetic instructions. The same final-result gate covers
`logand`, `logior`, `logxor` and `lognot` at their native Lisp boundaries,
as well as allocating `floor`, `ceiling`, `round`, `truncate` and integer `abs`.
Unary rounding of an integer and nonnegative `abs` retain existing objects and
are exempt; conversion of a large finite float to an integer is not exempt. Existing shift/power policy is separate.
Floating results are not subject to integer magnitude limits. Singleton `+` and
`*`, and singleton `logand`/`logior`/`logxor`, return an existing operand
without an arithmetic allocation or width gate. Singleton bitwise operands still
require integer type validation.

The check is **after the complete variadic operation**, not after each fold step.
GNU Emacs 30.1 permits an intermediate result to exceed the bound if the final
result is in range. For example, with `y = 2^127` and a width of 128,
`(+ y y (- y))` succeeds even though the first partial sum needs 129 bits.
Likewise, a preexisting `x = 2^200` can cancel to zero under the smaller binding:
`(+ x (- x))`, `(- x x)` and `(* x 0)` succeed; `(+ x 0)`, `(* x 1)`,
`(/ x 1)`, `(1+ x)` and `(1- x)` overflow.

This gate checks observable results before publishing a new Lisp object. It is
**not an allocation preflight**: owned input clones and temporary BigInt results
may allocate before validation. Infallible num-bigint allocation can still exhaust
memory. Do not impose an unrelated host allocation cap or reject every oversized
partial result in the name of this policy.

The broader policy remains incomplete: readers and other integer-producing adapters still need operator-specific
qualification. Width values are validated when an operation consumes them, not
when binding/assignment occurs. Named GNU overflow conditions are also incomplete.

Qualification includes a 21-form GNU batch oracle for final-result, cancellation,
float-selection and singleton exemptions, matching Rune Lisp regressions; seven
VM opcode groups validate width overflow independently. The oracle is bounded to
small (at most a few hundred-bit) values and does not stress resource exhaustion.
