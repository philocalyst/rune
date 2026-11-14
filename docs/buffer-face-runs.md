# Buffer face-color runs: owner-only publication slice

The owner resolves retained BUFFER text-property `face` values into plain scalar
FaceSpans for the currently laid-out visible range. This does not assume the
separate Lisp string-property/insertion pipeline is present. Document clones
retain authenticated property leases while resolving after the editor borrow is
released. No Lisp values, roots or Context enter GPUI or the frame DTO.

Supported color references: frame-local named Symbol/String faces, bounded
Symbol/String aliases, ordered face lists, anonymous foreground/background plists
and named/anonymous `:inherit` color fallbacks. Only raw foreground/background
colors are published; fonts/decorations/distant-color heuristics are not applied.
Non-nil inverse-video, default-face inheritance, duplicate anonymous attributes,
unknown faces/colors, and non-nil font-lock-face properties fail explicitly.
Global new-frame defaults, overlays, mouse faces, full font-lock/Org styling and
GNU redisplay parity are not claimed. Colors retain the previous scoped hex and
basic-name support, including small ASCII-byte color strings.

The bridge obtains a visible scalar interval from bounded geometry, then resolves
and paints only that slice. Revision DTOs include window/buffer identity, document
length, exact visible interval and exact resolved plain colors/errors. Recompute
before/after callbacks detects direct mutation of property lists, named face
vectors and color strings without relying on text or setter epochs. Error DTOs
permit corrective evaluation, and style-only changes preserve input authority.

Bounds are checked before accumulation/lookup: 100000 visible scalars, 4096
candidate property runs, 32768 resolver steps, depth32, 256-element list/alias
spines, 64KiB per string face name and 1MiB aggregate name BYTES. Native geometry
may make up to three bounded layout passes; no whole text or property prefix is
flattened for styling. Oversized/malformed/cyclic styles return static errors,
not formatted Lisp graphs. Headless FIFO/SVG/long-Org tests are not OS/window
input or original Org-mode workflow verification.
