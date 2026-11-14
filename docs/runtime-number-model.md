# Runtime number model: RT-02 foundation

## Boundary and ownership

`core::object::number` is the single owned numeric kernel. It has no evaluator,
argument-list, root-stack, or collector dependency beyond the final object
construction adapter. `Number<'ob>` remains the existing borrowed tagged view;
`NumberValue` is the owned value that can cross an arithmetic calculation.
`NumericError` represents recoverable arithmetic/domain failures before the
primitive adapter translates them into the existing condition boundary.

Every `NumberValue` converted into an `Object` is normalized. `Block::make_integer`
and `Context::make_integer` accept machine or arbitrary-precision integers and
select the canonical immediate/boxed representation. They do not extend any
borrow or transfer an object across heap domains. The old raw `i64::tag` is a
legacy unchecked storage API, not the new numeric construction contract.

## Implemented migration

* `NumberValue`, fixnum bounds, canonicalization, numeric conversions and all
  arithmetic/comparison implementations live in the object model, not `arith`.
  `arith` temporarily re-exports the names to keep existing callers building.
* `+`, `-`, `*`, unary negation and the increment/decrement adapters preserve
  exact integers, promoting and normalizing across the current tagged boundary.
* `/`, `%` and `mod` use fallible kernel operations. There are deliberately no
  infallible `Div`/`Rem` implementations for `NumberValue`.
* Float equality is IEEE exact, not approximate. Mixed ordering converts the
  float's integral part to an exact integer; the integer is never rounded to a
  double before comparing. NaNs remain unordered and infinities are explicit.
  Lisp comparison adapters validate operand numeric types only until a failed
  comparison, matching GNU's short circuit, while argument expressions are
  still evaluated normally. Separate typed helpers preserve VM-facing APIs.
* `max`/`min` preserve the first equal operand's numeric kind and signed zero,
  and propagate NaNs through the fold. When both operands are NaNs, the later
  NaN takes precedence, including its sign. `/=` has exactly two arguments. These
  operand rules were checked against a local GNU Emacs 30.1 executable.
* Bitwise operators accept arbitrary-precision integers. Arithmetic shifts use
  exact sign extension and a prospective integer-width check before allocating.
  The `integer-width` variable defaults to GNU's 65,536 bits; GNU's machine-sized
  exemption is retained. This is a numeric policy, not an unrelated host RAM cap.
* `logcount` counts one bits of nonnegative integers and zero bits of negative
  integers in infinite two's complement, including arbitrary-precision inputs.
  Its owned-kernel operation rejects floats rather than silently truncating.
* `expt` uses fallible, width-limited exponentiation by squaring in the owned
  kernel instead of an arbitrary limb cap and an adapter `unwrap`. Product
  bounds reject certain overflow before allocation and check ambiguous products
  at no more than one extra magnitude bit. Nonnegative integer powers of 0 and
  ±1 remain exact for arbitrary-size exponents; negative/float exponents retain
  floating semantics. `ash` and `expt` share the width-variable adapter.
  The adapter accepts the exact signed 64-bit machine domain, including
  bignums outside the physical fixnum range, and rejects both adjacent values
  instead of saturating oversized configuration integers.
* Float primitive adapters use the kernel conversion and fallible truncation
  boundaries instead of duplicate conversion rules.
* Unary `round` and `fround` select even midpoints, matching the existing exact
  divisor-rounding path rather than Rust's away-from-zero midpoint policy.
* `number-to-string` is a native one-argument numeric conversion using decimal
  integers and the shared default float formatter. It creates a new Lisp string
  and preserves numeric kind/sign/payload through default literal read-back.
  This primitive also accepts GNU's validated `float-output-format` precision
  policy. The policy is scoped to this conversion; recursive object-printer
  formatting still uses the default formatter.
* Default finite float printing follows GNU's precision search and `%g`
  notation rules, preserving exact read-back while avoiding Rust's long fixed
  decimal spellings. Custom `float-output-format` bindings affect only
  `number-to-string`, not general object display.
  The reference policies are Emacs 30.1's
  [float printer](https://github.com/emacs-mirror/emacs/blob/emacs-30.1/src/print.c)
  and [gnulib dtoastr](https://github.com/emacs-mirror/emacs/blob/emacs-30.1/lib/ftoastr.c).

## Deliberate limits and next interfaces

This does **not** claim R1, R2, R3, R4, R5 or R6 completion. The physical object
word still uses the old tag byte and a **56-bit signed fixnum payload**. The
constants in the number module describe that storage honestly. Low-three-bit
GNU tags and a 62-bit fixnum payload require the common-header/kind migration;
changing only a constant would corrupt pointers and integer values.

Arithmetic width policy now also gates final results of ordinary arithmetic,
bitwise operations, allocating rounding and negative integer `abs`; the precise
scope and allocation limitations are in [the width policy note](runtime-number-width-policy.md).
This is not a global integer-allocation restriction: GNU Emacs 30.1 permits
`string-to-number` and `read-from-string` to construct a 1,000-digit integer even
under `integer-width` 128. Reader allocation must not be gated indiscriminately.
This does not claim a generational collector, named numeric condition hierarchy,
raw-byte strings, cross-domain identity, or finalizer-aware BigInt accounting.

A future R1 change should replace the storage constants/checked `Fixnum` codec,
make raw constructors private/fallible, migrate remaining direct integer
producers to `make_integer`, and qualify heap-dump compatibility. A future R2/GC
change must preserve `Object<'ob>`/`Context` lifetimes and count BigInt limb bytes;
it should not move owned numeric policy back into evaluator-specific modules.

## Qualification

Latest recovered verification on checkpoint `98fbdf8` plus the finite-printer
change: four actual Rune float tests passed; the whole Rune unit suite passed
435 tests with two ignored, using the batch GNU backend and `PROPTEST_CASES=32`.
The default finite formatter matched GNU Emacs 30.1 in 5,648 cases, including
random finite values and subnormal/normal boundaries. An extracted formatter
also probed 100,000 deterministic bit patterns, checking exact round trips for
every finite case. The in-tree
matrix uses 4,096 patterns and bounds every output at 25 bytes.

These are test-target and bounded-oracle results, not a full Lisp bootstrap,
native-window test, exhaustive property run, million-case differential pass,
or roadmap completion gate. The earlier pending statuses below are historical;
the latest unit run covers nonignored regressions present in this checkout.

Kernel unit/property tests cover arbitrary i64 construction, exact mixed
comparisons, IEEE classes, arithmetic against a BigInt reference, zero division,
width-limited shifts, and canonical object allocation. Primitive tests exercise
mixed comparisons, bignum bitwise operations and shifts. These tests are evidence
only when run; no million-case differential pass or M2 roadmap gate is claimed.

The resumed validation ran six standalone tests extracted from the actual
owned kernel against warm `num-bigint`/`num-traits` artifacts. These passed,
including a 121-pair integer boundary matrix, 608 power/reference comparisons,
11,514 additional power/width-boundary comparisons using references no larger
than 317 bits, 2,049 machine-integer and 36 bignum popcount cases, and
extrema/NaN/signed-zero cases. Eleven extrema/inequality, thirteen power/width,
and nine popcount primitive expectations were checked against GNU Emacs 30.1.
Formatting and whitespace checks passed. The extraction deliberately
excluded GC adapters, interpreter tests, and proptests; it is not a full runtime
build or evidence that those excluded tests passed.

The subsequent source-only review checked the `defun` parser's qualified
`Rt<Env>`/`Context` handling and the build script's `logcount` symbol discovery.
It also added unary midpoint and signed-zero rounding regressions. Those new
rounding regressions were initially uncompiled and unrun during the
disk-capacity restriction. GNU's installed
documentation permits platform-dependent midpoint choices, so the even policy
is an explicit consistency choice, not a claim about every GNU platform.

## Subsequent source-only integration supplements

These changes were initially prepared without compilation; their nonignored
regressions are included in the latest unit run described above:

* Generic `Object` to `f64` conversion delegates to `Number::to_float`, so
  general numeric primitive parameters do not reject bignums or bypass kernel
  policy. GNU confirmed that huge integer-to-float conversion yields infinity.
  Separate strict `&LispFloat` parameters enforce GNU's float-only contracts for
  `fround`, `fceiling`, `ffloor`, `ftruncate`, `isnan`, and both `copysign`
  operands. Their rejection of integers is not delegated to numeric coercion.
* `most-positive-fixnum` and `most-negative-fixnum` initialize the actual current
  56-bit storage bounds required by bundled `subr.el`. `natnump` accepts
  nonnegative machine or arbitrary-precision integers and rejects floats.
* GNU documents its bound variables as read-only. This implementation initializes
  ordinary variables and does **not** yet enforce immutable value bindings.
  Separate variable-constant metadata is needed; the existing `is_const` flag
  also constrains function cells and cannot safely stand in for that metadata.
* Property-test fixnum strategies use the canonical bounds and direct ranges
  instead of filtering arbitrary i64 values with very low acceptance rates.
* Runtime floats and property-test fixtures share a formatter for Lisp's
  `1.0e+INF` and `0.0e+NaN` spellings, including signs. Rust's bare `inf`/`NaN`
  would be read as symbols. Added display/read-back tests preserve finite bits,
  signed zero, and quiet-NaN sign/payload. See [NaN payload semantics](runtime-nan-payloads.md)
  for the modulo-prefix rule and the signaling-NaN limitation.

Source-only formatting and apply checks do not qualify these new regressions.
The compatibility plan calls for normal/test builds with regenerated symbols,
no-bootstrap and bootstrapped tests, and interpreted/compiled GNU comparisons
before claiming integrated runtime compatibility.

V3 additionally validates the width setting through the owned kernel's signed
machine-domain conversion. Six boundary primitive expectations were prepared
alongside direct kernel conversion regressions. GNU binding-time rejection is
still not implemented: the adapter
rejects invalid settings when a migrated arithmetic primitive consumes them,
whereas GNU rejects the invalid assignment/binding even without arithmetic.

Subsequent GNU-only observations ran the 12-form conversion batch and 11
bootstrap invariants. They exposed the float-only parameter distinction above;
13 strict-float and general numeric-argument expectations then passed against
GNU Emacs 30.1. Their Rust qualification was pending at that stage. Earlier
frozen patch versions without these strict
signatures must not be mistaken for the corrected aggregate.
