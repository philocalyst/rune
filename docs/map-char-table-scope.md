# Bounded live sparse character-table mapping

`map-char-table` traverses parent-free tables with a nil default, the actual
representation constructed by unchanged `regexp-opt-charset`. Non-nil defaults
and parent tables explicitly reject: existing general character-table default
and inheritance lookup semantics are not qualified by this patch.

Non-nil entries are visited in ascending character order, with adjacent EQ
values combined into inclusive range conses; singleton keys are integers. The
range cons is reused, so callbacks retaining it must copy it to preserve the
observed endpoints, as in GNU. This slice updates that cons only for delivered
ranges; GNU may also mutate a retained range cons while skipping a nil tail, so
its exact post-return endpoint state is not qualified. Callback results are ignored and errors/nonlocal
exits stop traversal without replay. Future changes are observed by a fresh
bounded query after each callback; changes before the completed cursor are not
revisited. The table, function, current key/value and reused range cons are rooted
across callbacks and moving collection. No table lock or borrowed data snapshot
crosses evaluation.

Character indices are limited to GNU's 0..4194303 domain. Cumulative sparse scan
and callback work is capped at 65536; quit is polled between callbacks. These are
explicit resource limits, not total allocation/OOM, callback wall-clock
preemption, inherited/default-table or full keymap/dense-table guarantees.
