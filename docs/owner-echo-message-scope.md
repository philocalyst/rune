# Owner message requests and nil clearing

`message` formats before changing owner state. Each successful request replaces
bounded, owned Rust echo text for the validated selected logical terminal frame;
`message nil` actually clears that retained text, advances its revision, returns
nil, and does not format unused arguments. Empty formatted strings return the
empty string but clear the requested area. Formatting, allocation preparation,
size-limit and revision-exhaustion errors leave the previous state intact.

The record contains no Lisp pointer, HeapDomainValue, Context back-reference,
or hidden GC root. It survives moving collection, is shared by Envs through
the Context, and drops with that owner. Native snapshots own their copied bytes
and are observations, not authority or permission to publish after retirement.
The retained request limit is 16 MiB, not a total formatter allocation/time bound.

There is no attached echo-area renderer in the current logical terminal backend.
`current-message` therefore truthfully returns nil, like GNU batch terminals,
including when Lisp dynamically binds noninteractive to nil. The native requested
text/revision is real state and clearable, but is not misreported as painted text.
GUI echo painting, per-window/minibuffer redisplay, message hooks, *Messages* log
parity, inhibit-message behavior, and additional frame backends are not supplied
by this patch. Existing CLI `MESSAGE:` presentation is retained. Console I/O may
fail after state publication; an error there is not evidence that the owner
request did not take effect. Clearing does not erase historical logs/snapshots.
