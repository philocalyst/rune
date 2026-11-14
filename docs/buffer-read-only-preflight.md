# Owner read-only preflight

`barf-if-buffer-read-only` checks the current authoritative buffer's forwarded
`buffer-read-only` value without changing it. A non-nil dynamic
`inhibit-read-only`, or a non-nil `inhibit-read-only` text property at the checked
character, bypasses this preflight. Otherwise it signals structured
`buffer-read-only` carrying the real current buffer.

An explicit POSITION must be a fixnum (not a marker), even for writable buffers
or inhibited checks. Numeric bounds are checked only if the read-only check
actually needs character lookup; the endpoint has no character property.
Invalid types retain the original object in a structured condition without
recursively printing it. Buffer properties are inspected under shared Context
access with a 4096-pair plist traversal limit; malformed/cyclic/oversized property
spines fail closed without rendering arbitrary values. No callbacks or native
processes run and no buffer text, point, property, marker or flag is changed.

This is not an implementation of text-property read-only enforcement, all
editing modification hooks, category/default property inheritance, narrowing or
an implicit bypass of the editor's mutation protections. In particular, making
this preflight succeed does not itself grant a write through an owner gateway.
It supplies a real prerequisite used by original simple.el shell helpers, not
replacement shell output or a claim that all their marker dependencies exist.
