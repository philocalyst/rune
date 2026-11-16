# Reconstructed hash core: SOURCE ONLY, NOT RUN

Restored on published 3699a33 after cloud workspace loss. No recovered source
has been compiled or executed in this cloud. Earlier qualification is historical
and does not substitute for new check/tests and independent review.

Core stores actual SemanticKey(ObjCell,test)/ObjCell values, not a layout cast of
IndexMap<Object,Object>. Eq uses header identity; Eql adds bigint numeric equality
and float bit equality (same-bit NaN equal; signed zero distinct). Equal is
bounded iterative structural comparison and ASCII-compatible text/raw-byte
units; high raw bytes are distinct from Unicode. All resident structural keys
and queries are aggregate-validated before lookup/mutation. Mutating a resident
key's equality/hash content has no supported lookup guarantee. GC rehash does
not recurse through structural graphs; the pinned IndexMap rehash implementation
must be independently rechecked to perform no Eq callbacks.

Bounds: 65536 entries; key work/pending100000, depth256; single payload1MiB and
aggregate payload16MiB. Repeated DAG expansion is charged. Marker structural
keys reject (contextual equality differs), while Eq/Eql identity keys are real.
When merging InterpretedFn, explicitly reject it in Equal validation and add the
new weak-state dispatch arm; do not rely on wildcard opaque identity equality.
ByteFn Equal covers only currently stored fields; omitted GNU doc/interactive
metadata distinction is not claimed.

Strong, Key and Value weakness policies are genuine owner-local policies. Weak
entries are deferred after all strong roots in BOTH collector entrypoints.
Temporary cells mark only the activated opposite edge while stored cells remain
from-space throughout a least fixed point, including newly discovered weak
tables. Remove dead entries by cached index (not key Eq/Hash); adjust maphash
cursor for preceding removals; forward survivors and rehash before old space is
released. Immediates and audited global nonmoving objects are live. Equal-looking
replacements do not confer stored-object liveness. Native finalizers retain the
existing deferred-drop ordering, not cancellation/GC-table-borrow callbacks.
No weak-mode strong fallback, total GC work bound, OOM or mid-GC panic recovery.

Foreign weak table copies reject. Eq/Eql keys with nonportable header identity
reject; Equal keys transitively containing opaque table/channel/uninterned-symbol
identities reject. Trusted native constant allocation requires Strong and the
complete global graph ownership invariant. Legacy dump schema cannot preserve
strategies/weakness: reject before file output. These conservative policies are
NOT a resolution of the real normal-bootstrap failure already diagnosed before
workspace loss: fset globalized cl-generic-generalizers' empty Eql method cache,
then insertion of a local Unmoved Cons key failed. The guard must stay intact;
canonical owner runtime/function authority is a separate required architecture.

Five native strong-strategy and nine weak collector regression sources are
reconstructed, NOT RUN. Constructor/reader and four native consumer adaptations
are separate patches owned by VM/Text. Core source alone does not form a compiling
complete chain; no behavior or whole-source completion is claimed.
