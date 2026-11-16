# Rune channel timeout conversion boundary

`channel-send-timeout` and `channel-recv-timeout` are Rune-specific channel
operations, not GNU Emacs channel APIs. Finite negative seconds still mean
zero timeout; valid positive fractions use Rust Duration rounding unchanged.
A zero timeout still attempts one immediate operation, as before.

NaN, either infinity, and positive finite seconds outside Duration's native
range now return an ordinary static Lisp error before acquiring an operation
lease, copying a message or touching the queue. This deliberately changes the
old NaN/negative-infinity normalization-to-zero behavior and replaces positive
infinity/oversized-duration Rust panics. No GNU compatibility is asserted.

This conversion does not set an application maximum wait, cancel an admitted
operation, alter channel close semantics, reclaim queued endpoint cycles, or
contain arbitrary allocation/backend panics. Existing polling and scheduling
latency remain unchanged; timeout expiry is not a hard real-time guarantee.
A representable Duration is not proof of a representable Instant deadline or
OS wait interval, and this patch does not introduce such an absolute deadline.

Native tests cover finite policy, rejection boundaries, no queued-message
mutation, endpoint-family counts across moving GC, and continued endpoint use
following errors. These are execution claims only after a recorded successful
run on the exact candidate.
