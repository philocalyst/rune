# Binary concat and substring slice

The Lisp `concat` entry now preserves ByteString input, including FE/FF framing
bytes and embedded NUL. Mixing it with ASCII strings/character sequences is
supported, including an initial empty string accumulator. Mixing raw bytes with
non-ASCII Unicode strings or character sequences is explicitly rejected: Rune
cannot represent Emacs's mixed non-scalar multibyte characters safely yet.
Unicode-only concat retains its existing scalar behavior.

`substring` preserves ByteString representation and indexes those strings in
bytes, including negative indices. Unicode strings remain scalar-indexed. Both
validate bounds before allocating a result. Existing string-property limitations
are unchanged. This does not add binary regex, a coding-system implementation,
or fix reader octal-byte literal classification; those are separate prerequisites
for the actual mu4e server filter.
