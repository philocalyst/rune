# Owner special events

GNU Emacs 30.1 keyboard.c initializes special-event-map as a real global sparse
keymap, separately from ordinary and function-key maps. Rune initializes that
same Lisp-visible, GC-rooted authority before loading bindings.el.

The native owner dispatcher consults this map before ordinary key lookup. A
bound event updates last-input-event and invokes the existing no-argument
interactive command path once. It is consumed rather than inserted or dispatched
through the normal map; ordinary pending key prefixes remain intact. Callback
errors retain partial effects and propagate without fallback or replay. An
unbound event continues through normal dispatch. Contextual variable lookup
allows Lisp rebinding of the map.

This is a scoped native owner-event implementation, not the full GNU low-level
read_char/command_execute protocol. OS signal delivery, process signals,
unread-command-events ingestion, interactive argument prompts, and recording
special events in command history are not implemented or established by these
tests. GNU synthetic unread-command-events controls did not exercise its special
read_char branch. No OS window or signal-delivery success is claimed.
