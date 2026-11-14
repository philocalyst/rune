# Startup default-directory

Env creation captures the real working directory fallibly, before editor
allocation. It requires an absolute UTF-8 path and retains an immutable native
initialization seed with a trailing platform separator. Generated variable
initialization allocates its Lisp string only after the Env has been rooted.
There is no unrooted Lisp pointer in the seed, silent cwd fallback, lossy path
conversion, or second mutable default-directory store.

Local locate-file-internal no longer consults default-directory when the name
or all search directories are absolute. Thus an explicitly empty binding cannot
break an otherwise independent absolute search. Relative searches with an
explicitly empty binding fail with a clear unsupported-case error. GNU has a
fallback for that invalid-relative-base case; this slice does not imitate it.
This is startup initialization, not buffer-local default-directory forwarding
or visited-file directory association.
