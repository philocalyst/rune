# Bounded property-string reader

The actual `wid-edit.el` literal `#(" " 0 1 (invisible t))` is read as an
annotated string, not stripped or evaluated. The first element is a freshly read
String or ByteString, followed by START END PLIST triples. Unicode indices count
scalars; raw string indices count bytes. Property values retain ordinary parsed
object identity, and the existing traced string property root keeps them live
through moving collection. Every field is parsed under shared Context access;
no callback or collection occurs before validated property publication.

This initial slice accepts forward, ordered, non-overlapping intervals and
proper even property lists. Empty intervals and nil plists add no annotations.
It explicitly rejects overlapping/reversed intervals, pre-annotated first
strings, malformed lists and unsupported reader labels, rather than pretending
full GNU reader parity. GNU supports additional interval normalization, overlap
replacement, permissive plist cases and cyclic reader labels. Error classes/data
for these unsupported cases are not GNU-qualified.

The string is capped at 1 MiB before scalar scans, fields at 12289 (4096 triples),
and total plist traversal at 65536 cells. Existing reader depth128 and string
property run limits apply. Errors are static position diagnostics without
rendering arbitrary graphs. These limits do not guarantee total allocation,
OOM recovery, or a hard wall-clock bound.
