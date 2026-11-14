# Genuine startup function maps and bounded native fallback

GNU keyboard.c initializes function-key-map as a real sparse map and creates a
distinct terminal-local local-function-key-map whose parent is that original object.
Rune common owner initialization does the same for function-key-map and
local-function-key-map. The local map is owner-local for the one current logical
terminal, not buffer-local; multiple terminal objects/switching are not modeled.
Rebinding the global variable does not silently retarget the existing parent.
Both graphs remain traced Env roots, including across moving GC.

The actual native owner dispatcher consumes this authority: an ordinary active
binding wins; otherwise the current local function map (including its parent)
may map one input event to one vector/string event. Local overrides, parent
fallback and a translated active prefix work, and translated output is NOT fed
back through function-key translation. Translation uses contextual variable
lookup, so current owner binding/alias changes are observed. Paste/composition
literal routes are not passed through native key dispatch.

This is a deliberately bounded dispatch slice, not read-key-sequence parity.
Only outputs that resolve to a bound active command/prefix are supported. Empty,
multiple-event, callable/prefix translation definitions, malformed output and
unbound translated targets fail before any command or original-text fallback;
errors clear native pending prefix. String output inspection reads at most two
scalars, byte/vector length is checked before access, and output events must be
nonnegative u32-range integers or symbols. Existing keymap traversal budgets
remain in force. No callback is run while reading translation maps.

Input-decode-map, key-translation-map, multi-event suffix accumulation, callable
prompt translators, command-remapping tables and multiple terminal semantics
are NOT implemented by this patch. No OS keyboard/window or full Doom workflow
claim follows from headless native-owner tests or successful source loading.
