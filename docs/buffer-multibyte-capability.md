# Authenticated Unicode buffer mode

`enable-multibyte-characters` is read from the current live authenticated owner
buffer's storage capability, not an independently writable Env/global flag.
The sole RuntimeDocument representation is Unicode Rope, so every supported
buffer and its default report t. Alias reads use that same authority. The
ordinary initialized default slot is t, but contextual/default getters do not
trust a shadow slot. No Lisp value is stored in a worker or native capability.

As in GNU, direct value writes, dynamic bindings, default writes and makunbound
signal setting-constant, including original alias names in condition data.
Function-cell policy remains independent. `set-buffer-multibyte` with a non-nil
argument is a real no-op on an already-multibyte buffer and returns the original
argument, including arbitrary truthy values. NIL requests signal rune-unimplemented
before any text, point, undo, epoch or publication change: no genuine unibyte
buffer representation or conversion currently exists.

This does not implement unibyte buffer text/byte-position conversion, multiple
representation backends, complete buffer-local metadata/query functions, generic
local-variable support or all GNU editing/coding behavior. Existing unsupported
local-variable APIs are not made real by a writable shadow flag. Tests compare
supported direct-value behavior against GNU, verify owner authentication/liveness,
prove shadow values cannot change mode, and check rejected conversion does not
publish an edit.
