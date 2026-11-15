# Real Entity payload destruction and owner disconnect diagnostic

Run `headless_row_disconnect` with native support. A real Session owner thread
publishes bounded snapshot source. A foreground GPUI Entity owns the UI endpoint
and replies during Entity::update. Only after the owner accepts a correlation
token does the foreground task drop the final entity handle and run an actual
App update/effect cycle. The entity payload's Drop implementation explicitly
drops its endpoint FIRST, then emits a capacity-one nonblocking notification.
The owner waits for that notification, observes Disconnected on poll and
verifies the previously accepted token was revoked. It does not pre-close the
transaction. WeakEntity death is checked separately; it is not used as proof
that the payload destructor ran.

The actual normal run exited0 with HEADLESS_ROW_ENTITY_DISCONNECT_PASS. A
second run with `--hold-entity` intentionally retains the entity while awaiting
the owner. Its two-second destructor-notification wait fails; the application
then releases the entity and exits1 with `entity payload drop deadline`.
Both final executable runs were externally bounded by `timeout 8s`; neither
required external termination. Channel deadlines are not a general application
watchdog; external timeout remains needed if the foreground task never runs or
unrelated native startup stalls.

No native retained line, Window, font shaping, rasterization, painting or OS
input executes. The registry token42 remains diagnostic data, not a native
resource. This is an example-only lifecycle path, not production FIFO wiring.
The owner detects/revokes on observation; it does not establish synchronous
atomic revocation of a concurrently painting native object. Normal startup
and rendering remain unchanged. No display restriction is bypassed.
