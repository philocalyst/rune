# Bounded offset charset registration: scoped qualification

## Current results and build configuration

The integrated candidate based on published `3fdb511` passed a fresh guarded build
and two separately executed exact 84-test runs, primary and independent, each
with 85 phases including test listing. Build elapsed was 235.407 seconds; primary
and independent run elapsed were 447.781 and 504.351 seconds, including pin checks.
Both runs used the same qualified test ELF. Source, dependency and toolchain pins
remained unchanged. The selection comprises the prior 77 owner/runtime tests plus
seven registration tests, including safe real-GNU comparisons with pinned batch
inputs. Independent execution is not an independent rebuild.

Qualification used `cargo test --locked --offline --no-run --no-default-features
--bin rune -j2`, followed by exact-name tests. Default-feature, GUI and other
feature configurations were not qualified by these runs. No startup probe of
this charset candidate has been performed; these results do not establish
successful Doom startup or full conformance. Maps, subsets, supersets,
unification and codec consumption remain unsupported; preload skips are untouched.
Detailed remaining limitations and historical failed compilation are preserved below.

## Historical source-preparation notes

The following unbuilt/unqualified wording records statuses when each frozen
revision was prepared. The original failed compilation is preserved; this header
reports the subsequent corrected candidate's scoped results. Only this status
summary differs from the tested source snapshot.

# Experimental offset registration integrated on published 3fdb511

Source-only, unbuilt and unqualified. No startup or full charset conformance claim.

Implements the genuine 17-argument define-charset-internal ABI for bounded offset-method registrations, including actual owner-local ISO designation and Mule byte-count tables, priority insertion/tick and membership, stable IDs, independently traced canonical names and descriptor attributes, registry attributes with GNU alias/redefinition separation. Five native seed metadata entries initialize these private tables. iso-charset and charset-priority-list observe private authority rather than mutable public plist/list state. Existing descriptors retain copied code geometry.

New definitions prepend charset-list, insert normal IDs before the first current supplementary descriptor, append supplementary IDs, and increment the priority tick. Redefinition retains the ID/priority position and does not republish the public list. Designation slots formerly assigned to a redefined ID continue referring to that ID, including its new canonical name. A registry alias retarget does not delete the canonical descriptor's rooted name/attributes. No new process-global Lisp pointers.

All fallible reserve/validation precedes publication; fixed ISO table has 768 entries, Mule table 256, dynamic IDs and registry keys have separate 4096 limits, and dynamic publication has a separate 65536 budget. Both primitive unit setup and real GNU differential source are added but NOT executed. Alias publications retain their existing separate budget. Allocation failure remains process-level, not a Lisp rollback promise. Resource rejection is stronger atomicity than GNU partial publication.

Explicit limitations: no map-file/vector/subset/superset/unification implementation, no codec consumption of the new Mule tables, no set-charset-priority, no unused-ISO lookup, no private unibyte/preferred-head or special JIS/KSC shortcut IDs. The actual first Latin offset definition is representable but next 8859-2 map and subset preload remain blockers. Preload comments are untouched. GNU dump temporary map semantics remain absent. Full geometry error/type/condition parity is not claimed: unsigned cons codepoints unsupported; geometry requires exactly eight entries and consistent nonzero dimension; unsafe hole endpoints and unsupported ISO dimensions rejected; some conditions are local generic errors. Maps/parents are ignored only when a non-nil offset selects the offset method, matching native method precedence. Non-nil unification always fails before publication. Cyclic/improper cons plists are accepted without traversal.

No GNU probes of unsafe geometry or malformed endpoints are permitted. The proposed new real-GNU test uses complete tiny geometry, ordinary bounded offset registration, fresh uninterned names, alias/redefinition and valid ISO/Mule metadata in a disposable batch process. Existing batch backend and coordinated pinned guard required.

Revision 2: explicit staged-list Object type; 4095/4096/4097 descriptor admission, replacement at ID/key capacity, independent alias-key exhaustion, and canonical cyclic attributes retained across alias retarget and four moving collections are covered by new unexecuted tests. Boundary fixtures are synthesized directly, not claimed as 4096 full public registrations.

The pre-existing charset-list builtin forwarding/defvaralias divergence remains unchanged. Nil and t names are accepted as symbols through ObjectType associated constants, matching native CHECK_SYMBOL.

Integration on published 3fdb511 preserves owner-local function/feature authority and all published runtime/test changes. Registration v2 production/test code is unchanged; only the additive Env fields/initializers are composed with owner state. This candidate has not been built or run. Prior owner 77x2 evidence is baseline evidence only, not qualification of this integration.

Compile-correction revision: the first integrated build failed exit101 with16 compiler diagnostics; no focused test executed. The successor uses a fixed17argument native wrapper, passes unwrapped values into rooted-vector push, and adds fallible capacity-only reserve to Rt<Vec<T>>. No assertion, registration behavior, or test is removed. Build failure/evidence remain preserved. This successor is unbuilt and unqualified.
