# Internal frame parameters

The current logical Terminal frame backend initializes frame-internal-parameters
to the GNU Emacs 30.1 non-X build list, in order: name, parent-id, window-id.
GNU frame.c adds outer-window-id only under HAVE_X_WINDOWS; Rune has no such
frame backend or native X window identity authority.

This is a real mutable, dynamically bindable, owner-rooted Lisp list. Original
frame.el excludes its entries when cloning frame parameters or saving deleted
frames; the original frameset initializer maps them to :never session filters.
It does not create or expose OS window identifiers, implement cloning/session
restoration, or turn the independent GPUI window into a Lisp graphical frame.
