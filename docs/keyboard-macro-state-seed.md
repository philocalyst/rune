# Genuine keyboard-macro state seed

GNU macros.c initializes executing-kbd-macro to nil before a player is active.
Rune's ordinary owner initialization now does the same in traced Env storage.
The value is writable, dynamically bindable and alias-readable as in GNU;
source-aliasing the built-in variable is rejected. Defaults, unbinding and
nonlocal-exit restoration use the existing real Env machinery. Cyclic values
retain owner identity across collection; no Lisp value enters native workers.

This supplies state read by the unchanged original subr.el called-interactively-p.
It does not replace that function or pretend keyboard playback happened.
There is no keyboard-macro player/recorder in this patch. A future genuine player
must bind the actual macro object during playback and restore it on every exit.
Physical native keys do not set this flag. Native call-interactively/backtrace
markers and complete called-interactively-p/command-loop parity remain separate
work. No whole source catalog, Org, Doom or mu4e completion follows from this seed.
