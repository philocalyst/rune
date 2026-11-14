# Opt-in owner weight/slant request projection

This is a callback-free owner query, not frame publication or enabled styling.
`LispRuntimeHost::font_variant_runs` validates window/buffer membership, document
length and visible range against an owned snapshot, clones only the authenticated
document (retaining property leases), drops the snapshot, then resolves on that
same persistent Env/Context owner. The result contains only plain IDs, ranges,
metrics/capabilities, epochs and normal/bold/italic/combined enum requests.
No Lisp object, font descriptor or Context reaches a worker or UI.
Only the direct buffer `face` property is read. Any nonnil `font-lock-face`
property fails closed even alongside an explicit supported `face`; nil is allowed.
No overlay/global-default merge or automatic style invalidation is implemented.

Weight supports `normal`, `bold` and `unspecified`; slant supports `normal`,
`italic` and `unspecified`. Explicit normal is distinct from unspecified, so it
can override inherited bold/italic. Named faces, bounded face aliases, ordered
face lists and anonymous `:inherit` use first-specified precedence. The selected
frame's direct default face supplies the final baseline emphasis when present.
This is not global new-frame defaults, overlay styling or full GNU inheritance.

Unsupported weights/slants fail with static errors. Explicit family/foundry,
width/height/font/fontset overrides also fail, including those on the default
face: the current logical terminal frame cannot establish that they match the
separate measured native baseline. This deliberately rejects common fully
initialized GNU default-face metadata until that baseline authority is defined;
it must not be advertised as styled Org support. Missing untouched default
metadata falls back to normal. Other color/decoration fields are not projected
by this weight/slant-only API, and this does not change their existing resolver.

The request must be supported by the provided plain measured capabilities. That
record is not security or input authority; actual native plan admission must
still validate the live bank, identity and exact baseline. This query alone does not
convert these runs to per-Face.id plans, stamp a new display protocol, send them
to a worker, compare them automatically after commands, or activate FrameView.
Normal rendering and protocol 3 remain unchanged. A separate experimental
paired owner-font stream can consume the query; see paired-owner-font-stream.md.

Limits: 100000 visible scalars, 4096 property runs, 32768 resolver work units,
32 inheritance depth, 256 alias/list/property steps, 64KiB per string name and
1MiB aggregate name bytes. Length checks precede string hashing/copying; cyclic
or malformed inputs fail statically without formatting arbitrary Lisp graphs.

Tests execute the real persistent owner: bold+italic inheritance, explicit
normal overrides, list precedence, unavailable combined variant, direct vector
mutation followed by a Lisp error with unchanged text/input epochs, GC and exact
plain reprojection, cyclic plist repair, unsupported family/weight, unknown
string names versus the legitimate face named nil, and invalid document scope.
These tests use the minimal owner factory, not whole GNU bootstrap. There is no
OS window, paint, shaping, automatic invalidation or interactive Org claim.
