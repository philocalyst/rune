# CAR/CDR operand conditions

Native Lisp `car`/`cdr` and their bytecode opcodes validate the original Object without generic List conversion. NIL returns NIL; a Cons returns the original component, including dotted or cyclic tails, without traversal/copying. Every other tag signals `wrong-type-argument` with `(listp ORIGINAL-OPERAND)`. Signal data retains the original owner graph; cyclic vectors or ByteFn constants are not recursively formatted while creating the condition. Shared Context access excludes moving collection until the independently rooted condition is registered. There are no callbacks in operand validation.

Pure typed Rust `data::car(List)`/`cdr(List)` helpers remain available to already-validated callers. This does not globally change generic `TypeError` conversion or the taxonomy of `nth`, `setcar`, `setcdr`, other list operations, malformed bytecode, unsupported instructions, or arbitrary interpreted function bodies. Native arity prechecking and funcall argument evaluation remain their existing separate behavior.

The actual three-opcode two-argument bytecode reproducer requires the separately reviewed VM entry-checkpoint/trace-cleanup fix as well as this condition adapter. The condition adapter does not claim to repair VM rollback by itself, and no source/package success is inferred from a micro-reproducer.
