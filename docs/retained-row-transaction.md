# Dormant retained-row correlation and revocation

This module is not wired into an owner event, renderer or UI transport. It
stores plain scope, bounded source and revocation state; no native or Lisp
objects. It does not authenticate supplied owner data or producer claims.

## Non-reused request namespace

Construction is fallible `new()`, never Default or Clone. A checked static
AtomicU64 allocates a distinct incarnation for every transaction in this loaded
module's process lifetime. Exhaustion fails closed without wrapping or resetting.
Each request key has private incarnation and per-transaction counter fields.
The FULL key must travel through requests, completions and UI registries.
No u64-only request ID API remains. Recreating an owner with reset sequence,
source and local IDs therefore cannot accept an old transaction's reply.
This is not persistent/IPC/reloaded-library identity or a security capability.

One request can be pending. Matching completion consumes it on success, stale
source/scope or invalid zero UI token. A foreign/late key does not consume newer
pending work. IDs are never reused after cancellation. Failed admission leaves
current state untouched, including any accepted lease. Source is capped at
256 UTF-8 bytes /64 scalars and rejects controls, not all unsupported scripts.
Exact bytes are compared without Unicode normalization. Admission computes a checked
half-open source scalar span (start + scalar count), rejects u64 overflow before
reserving an ID or revoking a lease, and exposes that exact span on the request.
This does not validate the span against an actual buffer or grapheme boundary.

## Accepted correlation leases

Successful completion returns a cloneable token containing the full request
key, opaque UI registry number and shared atomic revocation bit. Starting a
valid replacement request, cancel, close and transaction Drop revoke all token
clones. Explicit `revalidate(fresh_scope, fresh_text)` preserves the accepted
lease only for identical source/scope; mismatch revokes it permanently.
The UI registry must key by FULL request key plus its local token, never reuse
a numeric UI token alone as authority. The old numeric token can equal a new
one without making the old lease live again.

Liveness means only "this transaction has not revoked the correlation lease."
It is NOT native font/TextSystem/Window liveness, source authentication, valid
geometry or permission to paint. A concurrent liveness read may race a later
revocation; this is not an atomic paint/owner transaction. Integration still
needs UI-domain serialized admission and paint-phase checks. Source range
bounds, current owner snapshot, style/viewport/font revisions and producer
provenance remain caller obligations. Callers must invoke invalidation when
state changes; the state machine cannot observe Session or native changes.

Tests cover recreation ABA and recycled UI numbers, parallel constructor
uniqueness, clone revocation on replacement/cancel/close/drop, every scope
field, exact source mismatch, duplicate/foreign completion and counter
exhaustion. No real owner/UI asynchronous pipeline or Window paint executes.
