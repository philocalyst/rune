# Bounded owner-local keymap copying

`copy-keymap` resolves a symbol input and copies actual sparse keymap spines,
binding cells, nested direct prefix maps, and vector storage. Parents remain
shared. Repeated prefix-map DAG occurrences are copied separately, matching GNU;
ordinary command/lambda Cons leaves and symbol alias edges remain shared.

New-style `menu-item` list spines are copied, with the command position's direct
prefix map copied recursively. Labels and property values remain shared. Old
string-label menu wrappers are copied, while ordinary command tails are shared;
a direct prefix-map tail is copied. This does not add menu lookup/dispatch
semantics to existing consumers. Prompt strings and ordinary leaf values retain
their identities and properties without globalizing or cloning owner objects.
Character-table keymap storage explicitly rejects; no dense CharTable parity is
claimed. Rune's existing `make-keymap` sparse-storage policy is unchanged.

An iterative task/value stack runs under shared Context access, with no callback,
evaluation or collection. Source pointers and fresh copy Objects cannot cross a
moving collection before the returned graph is published into its caller root.
No table/editor/global transfer lock or new unsafe operation is used. Cyclic
local spines, prefix definitions and menu spines reject with static errors;
shared parent graphs are not recursively traversed. Partial copies on an error
remain unreachable owner allocations for ordinary collection, not global data.

Task/value/work caps are 100000 and internal traversal depth is capped at 128.
The work cap counts repeated DAG expansion rather than unique source nodes.
These limits do not guarantee total allocation, OOM recovery, full malformed
keymap compatibility, arbitrary leaf graph rendering, or wall-clock preemption.
