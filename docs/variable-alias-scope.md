# Variable indirection, isolated candidate

Variable alias edges are owner-Env traced symbol slots, separate from values.
An edge names its immediate target. Resolution preserves later retargeting and
has a 100,000-link traversal budget. No collection or callback occurs while its
native identity set is live. This is not a global cross-Env alias namespace.

Lexical versus special binding decisions use the original name and existing
per-load declarations. Only dynamic storage resolves aliases. Dynamic unwind
captures the canonical base at entry, so retargeting an existing alias does not
redirect an already-installed unwind record. Owner-forwarded bindings retain
original-buffer targets. Clearing a global special flag does not erase local
load declarations or change lexical lambda parameter rules.

Known native/owner-forwarded source variables reject becoming aliases. Targets
may be constants; attempted writes preserve the original requested name in the
setting-constant payload. Void reads preserve that name too. Alias cycles are
rejected before publication. Generic buffer-local variables, GNU process-wide
alias state and user-facing overwrite warnings are not claimed by this stage.
String-valued documentation properties inherit through variable aliases; external
DOC offsets and documentation command-key substitution are explicitly unsupported. Existing native startup initialization is
privileged and intentionally does not resolve user-installed aliases.

Initialized DEFVAR uses resolved current/default storage, skips bound
initializers, marks the declared name special before initializer evaluation and
returns that name. No-initializer declarations retain per-load scope behavior.
