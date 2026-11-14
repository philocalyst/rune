# Live map-keymap traversal scope

`map-keymap` and `map-keymap-internal` traverse actual sparse keymap conses,
parents and embedded parents with rooted LIVE cursors. Prefix keymaps stored as
binding values remain leaves. Parent duplicates and explicit nil bindings are
visited; t definitions are reported as nil. Callback results are ignored.
Future rebinding/deletion, newly attached/detached parents and GC/reentrant
callbacks are observed without retaining native borrows across evaluation.
Raw vector table slots are visited in index order, including nil slots.

The third argument delegates sorting to the original GNU `map-keymap-sorted`
Lisp helper; normal product bootstrap loads that helper. Minimal unbootstrapped
owners must load the genuine helper before requesting sorting.

Traversal is bounded to 65536 steps/vector slots and 256 embedded parents, with
cooperative quit checks and no replay after callback errors. Invalid map input
signals wrong-type-argument. Character-table traversal explicitly errors rather
than inventing range semantics. Existing Rune `make-keymap` still uses sparse
storage, so GNU dense character-table range compression is NOT implemented by
this slice. No blanket full keymap, autoload, or GUI/Doom parity is claimed.

Unit oracle forms are expanded with GNU's actual macroexpand-all so isolated
no-bootstrap fixtures do not require fake lambda/push helper definitions.
