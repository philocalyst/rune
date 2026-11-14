# Property-bearing strings

`propertize` creates a same-storage-kind string with actual traced property runs.
Property values retain owner-local Lisp identity, including keymaps and conses.
New properties cover the entire string and override inherited properties;
duplicate incoming keys use the first value, qualified against GNU. Existing
strings/plists are not mutated. Empty strings retain identity and have no spans;
nonempty strings are copied even when no properties were supplied.

String property positions count Unicode scalars for multibyte strings and bytes
for unibyte strings. `text-properties-at` and `get-text-property` read real run
contents, preserve nil-valued and nonsymbol keys, and return nil at the endpoint.
The primitive reports odd property argument counts before validating STRING.

Limits are explicit, not GNU behavior: 8192 property arguments, 4096 source runs
and plist pairs, and one million merge key comparisons. Lookup and merge detect
cyclic/malformed plist spines and fail rather than looping. This is not a
universal allocation or wall-clock bound. Arbitrary property values may still
contain ordinary Lisp graph cycles; GC/clone handling belongs to the core.

Property ordering is not a public canonicalization guarantee. Category/default
property inheritance, character-property alias lists, string property mutation
and text-property interval navigation are separate work. Transformations must
preserve properties or explicitly reject property-bearing inputs; they must not
silently discard them. This feature is not a complete display/mode-line renderer.
