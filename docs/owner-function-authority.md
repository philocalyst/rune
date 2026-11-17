# Owner function authority: scoped verification and revision history

## Current verification status

Integrated owner-v4, based on published `033528f`, passed a fresh guarded build
and the same 77 focused tests twice, in primary and independent runs. Both runs
also passed the pinned GNU raw-lambda oracle. These are scoped regression results,
not full conformance. The runtime source below is identical to the tested candidate;
this verification summary is a subsequent documentation-only update.

The genuine normal-CLI startup retry exited 137. The run reported 103.261 seconds
elapsed, including post-exit checks. Its last output was `Loading gv...`, with no
completion marker and input pins unchanged.
Startup is NOT qualified. Exit 137 alone does not establish why it was killed.
No successful Doom startup, GUI workflow, or full GNU/Doom conformance is claimed.

Known limitations: Lisp-visible features and loader lifecycle are not yet unified;
interpreted closures retain Rune's Cons representation; arbitrary computed documentation
metadata and exact malformed-formal condition identity are incomplete; unsupported
owner-function/feature persistence fails closed; compiled ELC support remains unfinished.

## Historical implementation notes

The revision notes below retain their original preparation-time statuses. Their
“unbuilt”, “unqualified”, and “no tests run” statements are historical, not current
scoped-test status. Earlier failed attempts and technical limitations are preserved.

Based on exact6833751 runtime source plus5cbca docs content. Independent evaluator Env becomes authority for Lisp function cells, including aliases and NIL tombstones. Traced map retains original captured graphs; native global definitions are immutable fallback, not mutated by Lisp fset/unbind. All owner call/introspection paths must use the same resolver. Native cross-domain publication APIs and hash portability guards remain unchanged.

Loaded feature state moves to the same Env to avoid require seeing another namespace's feature without its functions. This explicitly changes prior cross-Env interned-feature sharing; each independently constructed Env is an independent evaluator namespace even if Context storage is shared. Persistence rejects nonempty owner function namespace before opening output, until versioned serialization is implemented. This prototype is not fully qualified or a full GNU runtime architecture claim.

Six new focused tests cover exact closure/capture identity+movingGC, aliases/redefinition/introspection, atomic cycle rejection, macros/interactive calls, builtin shadow/tombstones across Envs, uninterned alias roots and dump rejection. Existing autoload test reads canonical owner authority; feature-sharing test now asserts namespace isolation. Further VM-call, full-bootstrap, cyclic-capture and teardown coverage needed. No heavy build or test run yet.

## Revision3 safeguards (still unbuilt)

Registry is now a lazily allocated traced Eq weak-key table, not a strongly rooted symbol map. Existing ephemeron fixed point conditionally retains definitions while their name is independently reachable; uninterned name/function cycles alone must die. Interned symbols remain live. Added native-channel finalization witness, root-alias multi-round activation test, and post-unroot collection checks. Existing weak-table implementation is reused, but these integration tests are required before assuming correctness.

Revision2 also fixes resolved-autoload funcall/apply dispatch and rejects unchanged/autoload results, aligns exact256edge resolver/publication boundary, corrects inherited keymap cross-Env expectation, rejects legacy non-native function dump imports before allocation/interning/mutation, and uses available rooted-map APIs. Eight v1/v2 tests plus three v3 tests (11total) are proposed; no tests run. Export and import restrictions are explicit compatibility changes pending proper owner-aware persistence schema.

## Revision4 (unbuilt)

Native dump imports are accepted only for registered same-name native definitions matching the initialized global binding; noncanonical aliases/rebindings fail before mutation instead of being silently skipped. Export rejects feature-only evaluator state as well as function authority. Added regressions for native remapping and feature-only output noncreation. Direct, funcall and apply now follow changing autoload definitions up to the same64load limit; unchanged definitions fail. Added valid two-stage autoload coverage.14new proposed tests total, still no execution. v1/v2/v3 preserved.

## Revision5 integration (unbuilt)

v4 guarded build stopped exit101 with a single new-test E0502: unrooted symbol retained across mutable load_dump. No focused test ran. v5 roots that test symbol and rebinds after the call; no production ownership change from v4. Applies published3a1fb2e charset alias/offset changes exactly, including Env fields, atop the owner prototype. Compared to independently checkpointed3a1fb2e source, only owner-authority files/new tests/docs differ; charset.rs is identical. Previous failure logs and all revisions preserved. No rebuild or runtime success claim yet.

## Revision6 raw lambda execution (unbuilt)

v5 built successfully but first exact owner test exposed preexisting missing raw `(lambda ...)` invocation; focused0passed,49notrun, preserved. Original regression remains unchanged. v6 adds explicit dynamic-body Interpreter mode for raw calls: dynamic argument bindings, dynamic LET/LET*, FUNCTION returns original raw form without lexical capture. Stored fset identity is never normalized. Validates argument layout/arity into rooted temporary bindings before writes; roots return value and unwinds Env checkpoint for success/throw/error/partial binding failure. Existing lexical closure path remains unchanged. Six additional tests cover dynamic visibility across calls/LET, no caller lexical capture, exact raw definition mutation identity, GC/throw/error/partial failure unwind, arity atomicity and lexical/raw contrast.20owner tests now proposed, no new execution claim.

## Revision7 dynamic handler variable (unbuilt)

Raw dynamic-body condition-case now dynamically binds a non-nil handler variable, roots its payload/result, and restores the binding checkpoint on normal return, throw and handler error. NIL creates no binding. Existing lexical handler behavior remains unchanged. A separately defined reader regression checks dynamic visibility and GC plus normal/throw/error restoration.21owner tests proposed; prior v6 retained, no execution.

## Revision8 formal validation and GC stress (unbuilt)

Shared argument-list parser now rejects repeated optional markers, dangling rest and rest-marker names before binding; cyclic/improper lists retain existing iterator rejection. No parameter is permanently declared special. Added malformed/cyclic-formal atomicity test and heap-valued rest/return moving-GC test with formerly-unbound formal restoration.23owner tests proposed. v7 dynamic condition handler fix is retained. Original14tests unchanged and failed v5 evidence retained; no new build/test.

## V9 compile correction (unqualified)
V8 failed compilation with E0502 in FUNCTION: returning the initial borrowed form extended its Context borrow across lexical fallback mutation. V9 roots the same form before branching, then rebinds only for the dynamic return. No object normalization, assertion, or semantic change; all 23 owner regressions retained. V8 evidence preserved. No V9 build/test performed.

## Integrated033528f v2 source-only mode corrections
Built upon frozen integrated v1. Lisp eval nil now selects dynamic mode; existing native embedding eval API explicitly retains lexical default. Macro invocation temporarily binds lexical-binding from caller mode and unwinds before expansion evaluation. Dynamic FUNCTION now evaluates documentation and constructs existing Rune closure with nil environment, whose invocation uses dynamic formals/body. Raw fset objects remain untouched. Rune Cons closure representation is not GNU30 vector conformance. The prior Rune-specific car=lambda assertion is replaced by callable/noncapturing behavior, not dropped. Three additional regressions proposed. No build/runtime qualification. Owner feature and load lifecycle issues remain.

## Integrated v3 doc-only correction (unqualified)
Computed documentation with no following body receives an explicit nil body, preserving metadata without returning it. Added dynamic and lexical doc-only invocation regression. A/B/C expected outcomes now observed against pinned GNU30 (9, nil, t), but this Rune successor remains uncompiled. Feature/loader work remains separate.

## Integrated v4 safety correction (unqualified)
Doc-only body explicitly binds the rooted handle before constructing a nil tail. Non-string computed documentation fails closed after evaluating the documentation expression; it must never become executable body data. This is an explicit limitation versus GNU arbitrary documentation metadata, not full conformance. Added rejection/no-execution regression.
