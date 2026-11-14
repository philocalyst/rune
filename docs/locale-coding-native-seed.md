# Native locale coding seed and nil coding

GNU30.1 `src/coding.c` initializes `locale-coding-system` to nil. The original
`international/mule-cmds.el` locale setup later assigns language/locale coding
priorities. Rune now supplies that native nil seed as an ordinary contextual
Lisp variable, not a blanket UTF8 guess. Assignments and dynamic bindings remain
real, including supported strict `utf-8-unix` conversion. This is not completion
of GNU locale negotiation, Windows code pages, language environments or arbitrary
locale charset support. Unsupported explicit coding systems still fail clearly.

A nil argument to encode/decode-coding-string means no conversion: preserve the
input's text versus raw-byte storage and properties. NOCOPY non-nil reuses the
same validated String. Otherwise copy via the property-aware string helper,
with independent plist spines and shallow property values. Valid empty strings
reuse their existing header even when NOCOPY is nil, matching GNU. The existing 16 MiB
input limit applies before copy. `last-coding-system-used` becomes
`no-conversion`. That metadata update precedes the fallible copy; malformed
property-copy errors may therefore still change last-coding-system-used. This
is not an all-error-state atomicity guarantee. This differs from explicit `binary`/`no-conversion` coding, which
uses the existing byte conversion semantics; nil must not be routed to binary.

This faithfully supplies the native prerequisites original env.el uses when
encoding variable names and decoding environment values. Native process
initialization and genuine Lisp locale selection remain separate stages;
a successful unset-variable getenv is not proof of complete locale startup.
