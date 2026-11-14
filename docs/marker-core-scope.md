# Owner marker foundation (held prototype)

A LispMarker is a real owner-arena GC allocation with nonreused native identity,
not an integer position. Its native attachment names an authenticated buffer and
Session marker ID. Session is authoritative for position and edit transformation;
no copied-position cache is maintained in the Lisp heap. Bias is retained while
detached. Killing a target makes attachment observations detached through native
buffer and Context liveness witnesses; witnesses are not access credentials.

The header owns one non-clone stable-ID lifetime ticket through detachment and
reattachment. Attachment views own no retirement duty. Moving collection transfers
that ownership with the header. Dead-header destruction is deferred through the
existing native finalizer mechanism. Ticket Drop only marks a pre-registered
native Cell; it allocates nothing, holds no Lisp value or Session/document lease,
and acquires no owner, registry or collector borrow. Gateway entry drains the lifetime node's exact current registration. Native
relocation/detachment preserves the logical lifetime ID for sparse Undo records.
Explicit reset/bias/anchor eligibility still requires native history qualification. Owner teardown tombstones liveness before heap release.

A canonical buffer mark is rooted even when detached. Its origin-buffer root
lifetime is separate from its current target attachment: killing origin A drops
only that root and must not detach an externally retained mark now targeting B.
Ordinary point/copy markers are not canonical roots and die when unreachable.

Same-owner CloneIn preserves identity. Foreign/global/channel transfer rejects
explicitly; no inert shell, copied position, cross-owner pointer, silent NIL or
routine panic substitutes for a real marker. Dump output rejects before writing.
This is a compatibility restriction, not general marker transport/persistence.

Native PartialEq is identity only and is NOT an implementation of GNU Lisp equal,
which compares live target/position and treats detached markers alike. Production
release is held until bounded contextual Lisp equality routes and unsupported
native/hash equality boundaries are audited. No broad cyclic graph equality,
resource/OOM recovery, arbitrary mid-GC panic recovery or marker rendering claim
is made by this foundation.
