# Real mu owner-pump qualification (not full mu4e or Doom)

The ignored test `real_mu_owner_pump_preserves_binary_chunks_gc_and_final_sentinel`
requires the pinned official mu 1.12.9 binary and the three-message synthetic
Maildir/database. It does not replace or claim to load any upstream mu4e handler.
It uses a fixture byte-capture callback only to inspect the genuine owner boundary.

It starts real mu through the Lisp process API; sets binary process reads and UTF-8 process writes;
sends ping/find/view(mark-as-read nil)/quit; handles native filter callbacks with
GC, concatenates exact byte strings, and waits for the once-only final sentinel.
Afterward Rust verifies all five actual framed responses by their hexadecimal
byte length, validates UTF8 only inside payloads, reads each as a Lisp sexp,
and checks version, three search results, the Unicode subject and actual view
source path. A metadata/path view response is not MIME body rendering.

Dependencies are the genuine process object/transport/primitives, completed
metadata retention, VM owner event pump and binary concat/length primitives.
The actual mu4e-source load/filter and full Doom workflow remain separate tests.
No copied Lisp handler, fake feature, hardcoded version response, account sync,
message flag update, attachment extraction or network content retrieval is used.

Explicit configuration:

- RUNE_MU_BINARY: official matching mu wrapper/executable.
- RUNE_MU_FIXTURE: generated synthetic fixture root.
- RUNE_MU_HOME: exactly FIXTURE/muhome.
- HOME, XDG_CACHE_HOME, XDG_CONFIG_HOME: FIXTURE/home, cache, config respectively.
- PATH includes the GNU reference wrapper if running the broader test suite;
  RUNE_ELPROP_BACKEND=batch selects the existing GNU oracle for unrelated tests.

Run `cargo test --offline --locked
real_mu_owner_pump_preserves_binary_chunks_gc_and_final_sentinel -- --ignored`.
The normal suite intentionally leaves this external-fixture test ignored.
