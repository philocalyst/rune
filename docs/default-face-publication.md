# First native face-color publication slice

This candidate resolves the selected real owner frame's default face into plain
foreground/background colors and a whole-buffer scalar FaceSpan. Window default
background is also set, including empty buffers. No Lisp objects enter GPUI.

Publication compares input Session sequence separately from a plain style DTO
(setter generation, validated frame identity, exact recomputed color/error).
Mutable face-vector/string bypasses therefore do not depend on setter counters.
Style-only changes preserve input/IME authority; input CAS remains unchanged.
Invalid styles report publication errors but do not prevent corrective Lisp.

Initial color support: #RGB, #RRGGBB, black/white/red/green/blue/yellow/cyan/magenta,
and terminal unspecified-fg/bg sentinels. Other names/formats fail publication
rather than pretending to paint. Symbol default-face aliases are bounded; string
aliases and inherited defaults are not implemented. Text-property, overlay,
font-lock, per-run face/inheritance resolution, fonts and decorations are NOT
implemented by this slice. No OS/window screenshot or graphical correctness is
claimed; the joined test executes real owner FIFO and the headless SVG painter.
