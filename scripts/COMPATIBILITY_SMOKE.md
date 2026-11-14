# Synthetic Org / read-only-mail vertical-slice smoke

Run after every integration checkpoint against a **fresh exact-revision binary**:

```
cargo build --locked --bin rune --target-dir /YOUR/ISOLATED/TARGET
python3 scripts/compatibility_smoke.py \
  --rune /YOUR/ISOLATED/TARGET/debug/rune --revision "$(git rev-parse HEAD)" \
  --oracle /path/to/emacs --oracle-revision GNU30.1 \
  --timeout 10 --output /tmp/compatibility-smoke.json
```

The report gives each gate's exact form, expected `(value t)` outcome, protocol
status, raw bounded output, executable SHA256, fixture SHA256 before/after, count
summary and first-failure reproduction command. `--gate org-file-read` regenerates
only that gate's synthetic input for reproduction. `--lisp-dir PATH` adds an
explicit trusted library search directory to package gates. It does not download
packages. Source revision labels are caller assertions; binary hashes identify
what actually executed. A completed transport or matching error is **not** a pass.

No user mail or account is used. Every run generates a Unicode `.org` and a
2-message `.mbox`, all addresses under `example.invalid`, in a fresh temporary
directory. Each gate/runtime has a separate process and HOME. The forms read only
these fixtures, never invoke mail retrieval/send or visit links. Both input files
must retain their hashes. This is configuration hygiene and a fixed trusted
program, **not** an OS/network sandbox. Do not point the runner at untrusted code.
Temporary fixture paths in recorded forms expire; use the generated reproduction
command to regenerate them. Reports are local; nothing uploads automatically.

## Measurable milestones

1. **Core prerequisites:** actual Unicode buffer insertion and exact text query.
2. **Actual file import:** exact synthetic Org and mbox bytes reach owner buffers.
3. **Read-only browse safety:** mailbox can be read/moved through, editing signals
   `buffer-read-only`, buffer contents and disk fixture remain unchanged.
4. **Real library availability:** load installed Org and Rmail independently of
   the import gate, then initialize their modes in actual fixture-backed buffers.
   Mail-utils must extract the first synthetic Subject.
5. **Workflow (not measured yet):** Org heading navigation, TODO change, guarded
   save/reopen exact roundtrip; mailbox message selection/next/previous, MIME body
   display, rejection of editing without mailbox mutation.
6. **Native interaction (not measured yet):** actual window, keyboard/IME, wheel
   scrolling, mode/status visibility. Linked headless tests are a separate useful
   gate, not proof of OS/window behavior.

Steps 1–4 have executable gates. Steps 5–6 remain explicit unmeasured requirements
in JSON; even all current gates passing must not be called a complete Org/email
client. CLI exit 0 means selected **measured** Rune gates passed and fixtures were
unchanged. Missing GNU is recorded `not-run`, never agreement. Raw Rune stdin
bypasses bootstrap, while GNU `-Q` includes its standard environment; this is a
pipeline diagnosis, not equivalent-startup semantic proof. Full Org/Rmail must
also pass the intended bootstrapped/native path before a compatibility claim.

## First qualified checkpoint

Fresh exact `a867e16` normal binary, SHA256
`5e9b32110da2f7940cb3ff8d621d8e4ab2ab861d8be0635296da7ec49555387f`:
Rune **1/9** gates; GNU30.1 **9/9**; both fixtures unchanged. First Rune failure:
`org-file-read` => `(error void-function (insert-file-contents))`. Mailbox import
hits the same blocker. Independent Org/Rmail loads fail because those libraries
are absent from Rune's load-path. This is measured failure, not static inference.

Actionable ownership: text lane implements bounded owner file-import and later
Context-aware read-only forwarding; VM lane diagnoses actual upstream Org load
and bootstrap prerequisites; display lane owns native file/scroll/input tests;
mail lane owns bounded synthetic MIME parsing without network. Rerun the full
command after each accepted change and send the JSON first errors to those lanes.
The next target is a real file-backed owner/native plain-text slice today; full
Org/Rmail behavior by the next day is not established by these prerequisites.

Unit fixtures: `python3 -m unittest discover -s scripts/tests -p test_compatibility_smoke.py -v`.
These mock protocol/accounting tests do not replace the real checkpoint above.
