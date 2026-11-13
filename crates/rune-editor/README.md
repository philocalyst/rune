# Headless editor ownership foundation

A standalone native session core: buffer identity/MRU/liveness, window combination
tree with validated resize, independent view positions, typed keys/commands,
transactional error/panic boundary, persistent undo/redo, savepoints, and native
keyboard macros. All positions are zero-based Unicode scalar offsets.

`rope` enables the authoritative rune-text Snapshot backend, retaining text,
properties and markers together through edits/undo. `display` additionally exposes
its indexed immutable TextSnapshot adapter. StringDocument is a reference backend,
not a second production text store. Native insertion properties are explicit;
GNU stickiness, Lisp hooks/keymaps, minibuffers and frame protocols are not claimed.

## Parallel integration

This integrated crate is a root workspace member; text/display dependencies use
sibling crate paths. Neither renderer nor runtime source is modified here.

Test without shared targets:

```
CARGO_TARGET_DIR=/tmp/rune-editor-direct/target cargo test --offline --manifest-path crates/rune-editor/Cargo.toml --features display
```

## Mandatory legacy bridge work

Legacy Lisp BUFFERS/BufferData have NOT been migrated. Do not mirror text into two
authoritative stores. The runtime adapter must map stable IDs to owner-domain
objects and route edits to this single Snapshot authority. Convert Lisp one-based
positions at exactly one checked boundary; internal view/text/display offsets stay
zero-based. Current legacy position inconsistencies must not leak into this API.

PropSetId is an opaque identity, NOT a GC root or proof of cross-domain safety.
The owning Context/domain registry must retain/root all referenced Lisp property
values for every live snapshot, renderer preparation and undo root. Foreign domain
IDs cannot be dereferenced or silently reused. Release document/window borrows
before Lisp evaluation. Prepared renderer data must not contain live Lisp values.

Known boundaries: one native frame, cell geometry requiring frontend pixel
conversion, native typed bindings rather than GNU Lisp keymap representation,
configuration positions clamped rather than historical marker replay. Recursive
window traversal is depth/node bounded; history chain reclamation is iterative.
No claim of complete ED/WF milestones or integrated Lisp editor is made.
