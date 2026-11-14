# Plain buffer-string

`buffer-string` returns all text from the evaluator's current live owner buffer,
including Unicode, without moving point or changing content/modified state.
Materializing this requested full string is intentional; ordinary edits do not
use this path. Empty buffers return an empty string.

GNU buffer-string preserves text properties. Rune's LispString cannot yet store
them, so this implementation explicitly rejects buffers with nonempty property
values instead of silently stripping properties. This does not add narrowing,
raw-byte buffer storage, or propertied Lisp strings.
