# Bounded genuine list completion

`try-completion` scans literal lists of plain Unicode strings, symbols or simple
alist entries whose car is a name. It returns nil for no match, t when every
matching name is exactly the input, otherwise the longest common prefix. Prefix
boundaries are Unicode scalar boundaries. Case-sensitive Unicode and ASCII
case-insensitive matching are supported. ASCII case selection follows GNU's
preference for a complete match and then preservation of the input's case.
Duplicates and ignored invalid entries/improper tails are GNU-qualified.

`completion-ignore-case` and `completion-regexp-list` have real native nil seeds
in traced owner Env storage and are read contextually. Non-nil regexp filters,
predicate callbacks, non-list tables, annotated strings, raw-byte strings and
non-ASCII case-folded matching explicitly reject, rather than returning invented
completion results or losing text properties. No callback is run and match data
is not changed. This supplies real input to unchanged regexp-opt/rx source.

Each input/name is limited to 1 MiB, list traversal to 65536 cells, and candidate
bytes to 16 MiB. Cyclic spines fail closed with static errors; no arbitrary graph
is formatted. Borrowed names and address identities exist only during shared
Context access, with no evaluation/collection. These are explicit resource and
completion-subset limits, not full GNU interactive completion, regexp/predicate,
case-table, arbitrary table, property or OOM parity.
