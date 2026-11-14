# Scoped annotated string insertion

Unicode strings and ASCII unibyte strings can import their existing text-property
spans into the single authoritative editor buffer. Plist spines are copied;
property values retain Lisp identity. Relative scalar ranges (Unicode) or byte
ranges (ASCII, therefore identical scalar positions) become authenticated retained
property handles before one owner transaction publishes text, properties, marker
adjustments, point, dirty state and undo history together.

Preparation is callback-free under shared Context access, with source arguments
rooted on the Env stack. The owner validates target epoch, span order/bounds and
registry membership before publication; no registry work follows publication.
A failure for an argument cannot publish partial text for that argument. Earlier
arguments remain inserted if a later argument fails, as with ordinary GNU insert.

This does not add non-scalar/raw non-ASCII byte buffer storage, new destination
property-inheritance semantics, property modification hooks, read-only text-property
enforcement, narrowing, property-aware rendering or complete Org editing behavior.
Those remain separate obligations. Atomic preservation of data is not a claim
that every imported property has an implemented display or editing effect.
