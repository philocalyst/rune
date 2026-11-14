# Local executable predicate

`file-executable-p` accepts text and native Unix unibyte paths. On Linux it
asks `faccessat(AT_FDCWD, ..., X_OK, AT_EACCESS)` using effective credentials;
it does not infer access from permission bits or execute the file. Symlinks
are followed; inaccessible/missing/dangling paths return nil. Directories test
search access, as GNU does. Relative and empty names resolve against the owner
`default-directory`, never the host working directory. NUL signals structured `wrong-type-argument filenamep` before any native access
call. Invalid types signal `stringp` without rendering the original object. Each
borrowed path and the checked joined path are limited to 64 KiB before copying
or joining; this is a scoped allocation guard, not a universal memory budget. Absolute paths need no current buffer/default value.

This is a scoped local-path primitive: it does not add file-name handlers,
remote access, `~` expansion, alternate filesystem credentials or non-Linux
platform implementations. It is a point-in-time predicate, not a guarantee
against later filesystem changes or authorization to execute a file.

The authentic mu4e version check must still run its original Lisp
`shell-command-to-string`/`shell-command`/`call-process-shell-command` chain.
No fabricated version output or replacement mu4e handler is provided here.
