# Inhibited match-data publication

String-match's fourth non-nil argument now suppresses match-data publication for
both Unicode strings and raw ByteString matching. The returned match position
and all existing pattern/storage/bound rules are unchanged. Original subr.el's
string-match-p uses this argument, so predicate checks no longer overwrite live
captures used by original mu4e cookie/helper code.

The implementation simply leaves the original Env match-data Slot untouched;
it does not save unrooted Lisp pointers and restore them after matching. Native
regex execution is callback-free under shared Context access. Native regression
checks preserve the owner root's pointer and then force moving GC. GNU tests
cover Unicode captures, ASCII and high raw bytes, nil/truthy flag values, and
misses. This is not full GNU regexp syntax, case-table, marker-match-data or
encoding interoperability parity.
