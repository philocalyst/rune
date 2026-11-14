# External bytecode call frame restoration

A valid GNU-generated two-argument function `[stack-ref 1, car, return]`, called
with `7,8`, can fail while its physical VM stack contains `7,8,7`. That operand
span is not the original argument frame. Previously the outer error-trace path
asked `CallFrame::arg_slice` to assert argument metadata against that span and
panicked, hiding the actual error. Optional padding and rest packing make a
truncate-to-parameter-count workaround incorrect.

The external bytecode entry now retains all supplied arguments in distinct
traced root slots before changing depth, preparing parameters or collecting.
It records native frame metadata separately. Both the VM error trace and the
outer wrapper trace use original supplied arguments, not padding, packed rest
lists or operand temporaries. On normal Ok/Err returns, including setup errors,
the entry's exact argument slots, frame depth limit and argument metadata are
restored. Direct native callers' argument metadata is finalized after validating
the supplied physical span. Nested optimized VM frames retain their existing
unwind path. The stack invariant is not removed or weakened.

A successful return value is rooted independently before restoring operands;
nonlocal errors retain their existing independently owned PendingExit payloads.
The restore performs no callback, collection or allocation: the original stack
capacity is sufficient and no VM operation shrinks it. Supplied argument object
graphs are retained for this external call's duration. This restores stack slots,
not mutable Lisp object effects, filesystem effects or an application transaction.

Binding/catch cleanup keeps its existing behavior. In particular a failed
binding restoration retains its traced binding record instead of being forcibly
truncated to claim success. This is not Rust panic/unwind recovery, arbitrary
mid-GC recovery, a new CAR/CDR error taxonomy, or general malformed bytecode/function
compatibility. Separate opcode validation and runtime safety limits still apply.
