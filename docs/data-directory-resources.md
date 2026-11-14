# Explicit installation resources

Rune has no bundled GNU `etc` installation and does not guess a versioned system
or temporary GNU data path. At Env creation it captures `RUNE_DATA_DIRECTORY`,
if set, otherwise the first native path-list component of `EMACSDATA`. The
selected path must be nonempty, absolute, an existing directory, and readable
with the running process credentials. Invalid explicit configuration fails Env
creation with a static diagnostic. Paths have a 64 KiB limit and a trailing
native separator; invalid UTF-8 paths remain ByteString values, never lossy text.

The verified startup path is inserted into the rooted, special `data-directory`
variable during normal variable initialization. It is an ordinary writable and
dynamically bindable Lisp value afterward, not a live host-environment fallback.
As a built-in variable, its source name cannot be turned into a variable alias;
other names can alias it. Startup verification does not claim assets remain
present after later filesystem changes or that arbitrary Lisp assignments name
valid installations.

With neither configuration variable set, no installation exists to advertise:
`data-directory` remains unbound, and a read signals the ordinary void-variable
condition. Configure one of the explicit paths before starting Rune when loading
packages requiring installation assets. No nil, cwd, invented directory or fake
image is substituted. A compatibility probe may explicitly opt in an acquired
official resource directory as a recorded test input; this is not a hidden
compiled default or a claim those assets ship with Rune.

GNU additionally has compiled installation defaults, relocation discovery and
relative `EMACSDATA` handling. Those are not implemented here; relative paths
reject explicitly. Tests use exclusive synthetic directories and isolated child
environments, preserve raw native path bytes through moving GC, and never change
user files, the host cwd, or process-wide environment in a running test process.
