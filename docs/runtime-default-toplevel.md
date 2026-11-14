# Top-level value primitives for bootstrap

The two default-toplevel primitives query/update the outermost saved dynamic
binding, or the Env value when there is no such binding. The getter signals
structured void-variable for an unbound top-level slot even when a current
binding exists. The setter validates before mutation, preserves the current
binding, and returns NIL. Values remain in existing traced Env/binding slots.

Four GNU comparisons cover binding preservation, missing saved defaults,
constant getters and moving-GC retention. These primitives unblock bundled
custom-initialize-default, allowing actual bootstrap through window.el.

This is the ordinary global/dynamic-variable subset, not complete buffer-local
or automatically buffer-local default semantics. Owner-forwarded variables need
separate contextual routing and targeted-binding metadata. No file/GUI/package
readiness follows from defining these primitives. A bounded bootstrap run still
needs a verified exit and next-failure diagnosis, not a timeout-as-success claim.
