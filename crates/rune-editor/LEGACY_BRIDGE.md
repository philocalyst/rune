# Runtime/event migration contract (not yet implemented)

## One owner and one document

1. Add a runtime-owned registry mapping live LispBuffer identity to editor BufferId.
   Check owning Context/domain on every lookup. Never accept a foreign domain ID.
2. Replace legacy BufferData text/property ownership with access to the session's
   authoritative RopeDocument Snapshot. Do NOT mirror edits into two text stores.
3. Route legacy insertion/deletion/property operations through one owner-thread
   transaction. Existing global BUFFERS and Env CurrentBuffer cached OpenBuffer
   guards cannot remain an independent mutable document authority.
4. Release cached buffer/document/window guards before evaluating hooks or Lisp
   commands. Root the command and results in their owning Context. Reacquire by
   stable live identity after callbacks, because a hook may kill/switch buffers.
5. Root all Lisp property values referenced by every retained snapshot/undo root
   in an owning-domain registry. Numeric PropSetId is NOT a root, capability,
   transferable Lisp value or proof of GC safety. Renderer receives prepared
   native faces, not registry values or Lisp pointers.

## Coordinate and event boundary

Use from_lisp_point/to_lisp_point exactly once at the Lisp boundary. They validate
one-based scalar positions including point-max and do not clamp invalid signed
values or cast bytes as characters. Resolve markers and validate Lisp numeric
kinds in the owner runtime first. Internal session/text/display positions remain
zero-based Unicode scalar offsets, not grapheme/byte/visual columns. Existing
legacy inconsistencies require migration, not a second conversion inside text.

UI key adapters must preserve modifiers and distinguish text input from control
keys; do not silently strip unsupported Alt/Super modifiers or substitute a
physical key label for committed Unicode text. Native Key/Command bindings are
not GNU Lisp keymap representation, translation maps or call-interactively.
Lisp-backed command dispatch needs a two-phase resolve/evaluate/reconcile owner
adapter outside session borrows, with hooks and normal condition handling.

## Publication and tests required before application wiring

Publish BufferSnapshot::display_snapshot, never a raw text-root revision. Editor
publication epoch advances on edit/undo/redo independently of historical content
versions used for savepoints. Prepare pixel geometry from cell rects with actual
frontend metrics; do not treat cells as logical pixels. Present immutable roots.

Required integration tests (runtime owner must implement): foreign-domain lookup
rejected; property value remains live through GC after edit/undo/redo; callback
kills selected buffer without dangling OpenBuffer; Lisp insert and native command
observe same authoritative text; one-based Unicode endpoints round trip; hook
failure reconciles current/selected buffer; UI committed Unicode reaches correct
window; stale publication cannot replace newer undo/branch output. Existing
boundary source tests only establish conversion arithmetic, not this runtime
bridge or GC correctness. No live Lisp integration is claimed here.

Committed IME/paste payloads now have a distinct native `InputEvent::Text` path:
insert the complete Unicode payload as one transaction/undo entry, rather than
translate its characters into physical key events. This cancels a pending native
prefix explicitly. Frontends must choose this event only after text commitment;
preedit composition is frontend state, not repeated committed buffer insertion.

Native frontends should submit `TextForWindow` with the originating focused
WindowId. The session rejects late commitments after intervening window focus
changes as StaleInput, rather than silently inserting into the newly focused
buffer. Do not blindly retry into current focus. Untargeted Text remains for
trusted headless/current-focus callers; composition cancellation/commit policy
belongs to the frontend, not text-buffer mutation.

Targeted commits capture BOTH WindowId and BufferId: a buffer switch can retain
window identity while changing its document. Native TextForWindow therefore
requires the originating buffer as well, and rejects a mismatched live pair.
This is identity validation, not a composition-generation token: frontend must
still cancel old composition on focus/buffer lifecycle changes (including ABA
switch-away-and-back), and resolve caret/preedit policy explicitly.
