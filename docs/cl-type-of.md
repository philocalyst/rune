# Representation-specific cl-type-of

This implements the constant-work GNU30 `cl-type-of` query for Rune's native
object representations, independently of `type-of`. Integers distinguish
fixnum/bignum, canonical nil/t distinguish null/boolean, native builtins report
primitive-function, and actual bytecode functions report byte-code-function.
Record class descriptors are decoded one level only, retaining the exact name
object identity. No class hierarchy is fabricated or installed by this query.

Rune currently represents interpreted Lisp functions as conses. They therefore
report cons, including quoted lambda/closure-shaped data, rather than guessing
that a cons is GNU30's vector-like interpreted-function representation. Special
forms are evaluator syntax rather than SubrFn objects, so no native special-form
object classification is advertised. Channels retain Rune-specific type names;
there is no GNU channel counterpart. An empty internal record is rejected with
a static error rather than panicking; Lisp record constructors require a tag.

The query performs no graph traversal, formatting, callbacks, collection, or
native editor access. Returned descriptor/name edges remain in the same owner
arena and are immediately published by the ordinary defun call boundary.
