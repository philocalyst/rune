# Search case-fold scope

`case-fold-search` has the GNU default `t`, accepts ordinary truthy Lisp values,
and is read by text and byte `string-match`. Dynamic nil bindings disable case
folding and restore it afterward. Text uses the regex engine's Unicode folding;
unibyte matching folds ASCII only, not raw high-bit bytes.

This stage stores the value in the persistent Env's dynamic variable namespace.
GNU makes assignment automatically buffer-local; that per-buffer forwarding and
custom case tables are NOT implemented here. Switching buffers does not establish
independent case-fold values yet. This is a real search setting with a documented
buffer-local gap, not a complete GNU variable implementation.
