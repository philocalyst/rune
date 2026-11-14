# Bounded genuine list completion

`try-completion` scans literal lists of strings, symbols or simple alist entries
whose car is a name. It returns nil for no match, t when every matching name is
exactly the input, otherwise the longest common prefix. Duplicates and ignored
invalid entries/improper tails are GNU-qualified. Case selection prefers a
complete match and then preservation of the input's case, following GNU.

Unicode scalars and raw bytes are compared as distinct internal units. ASCII
bytes share ASCII scalar identity; high raw bytes never become Latin-1 or UTF8
Unicode characters, even when their byte sequence is valid UTF8. Prefix lengths
count scalars for Unicode storage and bytes for ByteString storage. The selected
string candidate's original storage supplies the result, using the existing preserving
substring helper. Input annotations do not influence matching; output prefix
properties come from the selected candidate with independent plist spines and
shallow property-value identity. A property preflight bounds total copied plist
cells to 65536 and rejects malformed/cyclic spines without graph formatting.
Existing string-copy per-run limits also apply.

`completion-ignore-case` and `completion-regexp-list` have real nil seeds in
traced owner Env storage and are read contextually. ASCII case folding is
supported for both storage kinds; high raw bytes retain identity. Non-ASCII
Unicode case tables, non-nil regexp filters, predicate callbacks and non-list
tables explicitly reject rather than return invented results. No callback runs
and match data is not changed. This feeds unchanged regexp-opt/rx source; reverse
in its suffix branch can produce genuine ByteString candidates.

Input/names have O(1) byte prechecks at 1 MiB, list traversal has a 65536-cell cap,
and candidate bytes a 16 MiB cap. Borrowed source names/property descriptors and
address identities exist only during shared Context access, with no evaluation
or collection; returned prefixes are owned Lisp values. These are explicit
subset/resource policies, not full GNU interactive completion, arbitrary table,
callback, case-table, total allocation, OOM or hard wall-clock guarantees.

Symbol-name candidates do not retain their original string-storage representation;
returned prefix storage may differ from GNU Emacs for symbol candidates.
