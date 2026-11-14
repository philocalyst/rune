# Owner face metadata scope

GNU faces.el remains the high-level implementation. The native layer stores real
20-slot face vectors in the owner Env: new-frame defaults use the GNU-layout
hash entries `(id . vector)`, and canonical live frames have independently rooted
face tables. Attribute writes invalidate the owner matching generation. Logical
headless frames report terminal/non-GUI capabilities; geometry comes from the
same Session window tree, not an invented physical display.

Make/predicate nil selectors address global defaults. Get/set nil selectors use
the selected frame; t addresses defaults, and setter 0 addresses all live frames
plus defaults. Repeated make returns the same existing vector and resets its
attributes. Face alias lookup and Symbol identity are preserved.

One surprising GNU 30.1 behavior is deliberately retained and oracle-tested:
first frame-local internal-make-lisp-face through an alias uses resolved lookup
but inserts under the original alias key. Subsequent canonical/alias predicates
can therefore both miss that new local vector. This is not labeled a corrected
canonical insertion.

Dead-frame tables remain rooted until their Env is dropped. All frame-specific
entry points validate liveness/owner before lookup, so retained tables do not
resurrect a frame or authorize access to a closed/foreign frame. Earlier release
of dead-frame metadata is a future resource/lifecycle improvement.

Advanced font entities, complex underline/box forms, image stipples and some
attribute/condition semantics remain explicitly unsupported. These metadata
primitives do not yet publish resolved face spans/fonts into native paint.
Neither full faces.el/package loading nor OS window rendering is implied by the
unit tests. Actual package probes are tracked separately.
