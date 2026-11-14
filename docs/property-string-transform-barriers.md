# Temporary property-string transform barriers

Until true property-aware formatting/readable serialization is implemented,
`format`, `format-message`, `message`, and `prin1-to-string` reject property-bearing
controls and rendered values, including nested container values. Unused format
arguments are not rendered and remain allowed. Their callback-free Rust render
holds the global symbol gate across validation and output construction, excluding
concurrent global hash insertion. The traversal has a 100,000-node/task and
256-depth cap and rejects cycles. It is a conservative barrier, not full GNU
readable-printer parity; diagnostic internal Rust Display is not serialization.
No universal peak-allocation or hard wall-clock bound is claimed.

String case conversion and `replace-match` fail closed for annotated inputs
until their scalar/expansion mapping preserves runs. Plain-string/character Rust
helpers remain unchanged. Regexp quoting retains the original string (and its
properties) when no quoting is needed, as GNU does; rebuilt quoted strings
intentionally lose properties. Unicode and raw unibyte storage stay distinct.
Copying coding/base64 conversions intentionally discard properties in GNU too,
so they are not blanket-rejected. Supported coding NOCOPY reuse returns the
original object and keeps its properties; no new universal identity guarantee
is made for encoding/decoding.
