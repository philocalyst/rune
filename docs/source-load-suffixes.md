# Source-only load suffixes

The initialized owner variables are `load-suffixes` = (".el") and
`load-file-rep-suffixes` = (""). `get-load-suffixes` returns their Cartesian
product in outer-format/inner-representation order, preserving duplicates.
The real loader consumes that same configured candidate order before its
existing bare-file fallback; NOSUFFIX still bypasses suffix search.
`locate-library` can therefore discover genuine source libraries without
advertising a bytecode, native-module or compression backend.

This supported subset accepts text/UTF8 byte-string entries, ignores improper
list tails as GNU does, and rejects cycles/budget overflow without formatting
arbitrary graphs. Each list/product has a 4096-entry limit, each entry a 4096-byte
limit, and the final product a 1 MiB byte limit. Property-bearing entries reject rather than silently losing spans.
Non-string sequence coercions
accepted by GNU concat are not implemented here. Nonempty representation suffixes
and configured .elc/.eln/.so/.dll/.dylib/.gz formats reject explicitly. Other
configured extensions identify source-text files only. Existing compiled-file
rejection remains in force. These are scoped resource/format policies, not full
GNU installation-format, handler, remote-file, compressed-load or OOM guarantees.

Tests compare supported API ordering/duplicates/nil/improper tails against GNU
and execute real competing .lisp/.el source files to verify actual loader order.
Raw loader fixtures now initialize ordinary variables as production does.
