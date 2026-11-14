# Structured codec argument boundaries

The public encode/decode-coding-string wrappers validate the String argument
before the coding-system argument, matching GNU. Invalid String and non-symbol
coding arguments signal structured wrong-type-argument with stringp/symbolp and
the original object; they never stringify arbitrary invalid graphs. Cyclic cons,
vector and bytecode-constant graphs therefore cannot trigger recursive diagnostic
rendering through these boundaries. The private input_bytes fallback also uses a
static error, not recursive object formatting.

This changes type-error safety and precedence only. Unsupported explicit coding
symbols still produce the existing truthful supported-subset error, not a claim
that a GNU-valid charset is invalid. Supported conversions, property/copy rules,
resource limits and last-coding-system-used failure side effects are unchanged.
No host locale negotiation, coding destination buffers or package completion is
implied. Tests independently compare GNU predicate precedence and cyclic object
identity without serializing the graphs themselves.
