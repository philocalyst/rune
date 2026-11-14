# Bounded local locate-file-internal

Supports the actual Doom startup two-argument source-file search and local
optional suffix lists. Search preserves directory order, then suffix order;
suffixes append to the whole filename. Empty PATH and nil entries use
`default-directory`, relative entries resolve there, and results are absolute
lexically expanded UTF-8 names. Directories are skipped. Local regular readable
files, including symlink spellings, are eligible; missing candidates return nil.

Limits: 1024 path entries, 128 suffixes, 65536 candidate checks, and 64 KiB per
individual filename/directory/suffix. Malformed or cyclic lists fail rather than
looping indefinitely. Custom predicates/access-mode integers, file handlers,
remote paths and home-name expansion are not implemented. Non-nil predicates
are rejected explicitly. No canonicalization or protection against hostile
filesystem races is claimed; this read-only lookup does not open for writing.
Buffer-local default-directory remains a separate forwarding requirement.
