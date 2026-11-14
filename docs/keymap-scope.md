# Scoped sparse keymap implementation

This implements direct sparse Lisp maps, string/vector events, prefix and parent
lookup, symbol-function map aliases, explicit nil shadowing, default events, and
no-argument interpreted interactive commands. It is not full Doom compatibility.
Local/minor/overriding maps, menus/remapping, keyboard macros, interactive prompts,
and bytecompiled interactive metadata are not implemented. `make-keymap` uses
sparse storage, not GNU's full char-table representation.

Mutable direct map function definitions live in the persistent Env's traced
registry. Symbol alias edges continue through the immutable global function path.
A direct-map assignment reserves registry capacity before retiring the previous
global callable and inserting the owner root. Normal replacements publish globally
successfully before removing the old root. Invalid/cyclic/depth-exhausted map alias
resolution fails before publication. A successful replacement clears native prefix
state, including when the editor sequence did not change.

This is deliberately owner-local, not general function shadowing or namespace
isolation. Another Env does not acquire an owner's direct map; a later global
function replacement from another Env can disagree with the original owner's
local map query. Use is scoped to one persistent owner, not arbitrary concurrent
GNU function namespaces. Direct uninterned map names are traced across GC, but
alias edges to uninterned map symbols fail closed because global cloning changes
the target identity.

GNU oracle tests qualify the supported map operations and modifier encoding.
Rune's preexisting global fset backend rejects some definitions GNU permits, such
as integer definitions; failure-atomicity tests do not claim parity for those.
Native UI hookup is a separate patch and has no OS/window verification claim.
