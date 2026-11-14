# Truthful operating-system startup value

Fresh Env construction captures the compiled Rust target's operating-system
name as native-only input. Rooted builtin initialization interns the matching
GNU system-type symbol, rather than installing a Darwin string on every host.
Unknown mappings fail Env construction before editor/session initialization.

The Linux value and symbol type are compared against real GNU Emacs. Other
mapping branches are pure mapping tests, not cross-platform runtime/GUI tests.
This changes no emacs-version compatibility claim, window-system/frame backend,
CPU architecture, system-configuration, build configuration or GUI capability.
Ordinary variable rebinding and explicit dump restoration remain possible; this
is a startup default, not an immutable substitute for every host capability.
