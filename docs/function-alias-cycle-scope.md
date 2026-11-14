# Function alias publication and resolution

Ordinary function alias edges are validated while holding the existing global
symbol publication mutex. Self/cyclic chains fail before cloning or replacing
an old definition. Owner FSET/DEFALIAS convert the scalar rejection into a named
cyclic-function-indirection condition only after releasing that mutex. Clearing
a function cell cannot add an alias edge and does not require the writer lock.

Core call resolution is iterative, identity based, and bounded at 100,000 alias
links. The existing public indirect-function/functionp tortoise-hare traversal
also caps fast-cursor work at 100,000 steps, so continually changing writer
snapshots cannot keep it running indefinitely.
Legacy cycles, exhausted traversal budget, or allocation failure fail closed as
an unresolved function. This does not promise the GNU cycle condition on every
legacy lookup. Concurrent readers can observe a mixed snapshot and fail closed;
this is not a linearizable multi-cell read or permission to reclaim old global
function allocations while readers use them.

The bytecode FSET instruction uses the owner-aware function namespace, returns
the supplied definition, and preserves mutable owner keymap identity. Normal
symbol aliases remain global; direct keymaps remain scoped to their owner Env.
Existing uninterned global-clone identity limitations are not resolved here.
The separate graph-copy preflight/fallible rollback protect recursive function
payload copying; this alias check is not a general cyclic-object serializer or
resource-completeness claim.
