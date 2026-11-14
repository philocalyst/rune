# Actual Doom + mu4e benchmark (not a substitute viewer)

The acceptance benchmark is **real Doom Emacs and actual upstream mu4e**, with
synthetic mail only. The smaller compatibility smoke and standalone MIME preview
are prerequisites/interim slices, never substitutes or evidence of this passing.

Pinned reference acquired locally:
- Doom official `doomemacs/doomemacs` commit `7ccac7f643ad67baa25b216717cf8617a0903c69`.
- Official modules submodule `doomemacs/modules` commit `f054be8fcdd2a0446eb8259d76dd4f6f76acc0b7`.
- GNU Emacs 30.1, official Debian `30.1+1-6+deb13u1` package.
- Matched **mu 1.12.9 / mu4e 1.12.9**, official Debian `1.12.9-1` packages.
- Benchmark profile: pinned official `static/init.example.el`, bare `mu4e`
  enabled under `:email`; **not Gmail**, no accounts/credentials. This is a
  defined default-plus-mu4e profile, not every mutually incompatible Doom module
  or an unknown user's private configuration.

## Setup/reference gate

Clone official Doom with its pinned submodules. Keep all HOME, DOOMDIR and
DOOMLOCALDIR state in a dedicated benchmark directory, never the user's setup.
Copy the official config/package examples; enable bare mu4e in the init example.
Then run the real official installer, preserving a bounded log:

```
HOME=/BENCH/home DOOMDIR=/BENCH/config DOOMLOCALDIR=/BENCH/local \
EMACS=/path/to/emacs timeout 900 /DOOM/bin/doom --force --verbose \
  install --no-config --no-env
```

`--no-config` preserves the explicit benchmark profile; `--no-env` avoids capturing
an unrelated environment. Installation downloads the real profile's packages;
record their checked-out revisions and installer exit status. CLI `--help`, an
`early-init.el` load, a partial package cache, or a synthetic `provide` is **not**
a full setup or boot pass. No source rewrites or feature mocks are permitted.

Official mu/mu4e must be installed/extracted with matching versions. The separate
synthetic Maildir driver must use an isolated mu database, disable retrieval,
automatic read marking, HTML/images/remote content, and verify source hashes.
Its GNU reference checks actual process replies, headers search/next/previous,
plain MIME body display, and edit rejection in headers and view buffers. A mu
protocol response alone does not prove message view: mu4e/Gnus actually reads and
decodes the message file.

## Continuous executable gates

```
python3 scripts/doom_mu4e_smoke.py \
  --doom-root /DOOM --doom-config /BENCH/config --doom-local /BENCH/local \
  --gnu /path/to/emacs --rune /path/to/fresh/rune --revision ROOT_PLUS_PATCH_ID \
  --mu-driver /SYNTHETIC/gnu-mu4e-smoke.el --mu-home /SYNTHETIC/muhome \
  --mu-binary /TOOLS/bin/mu --mu-lisp /TOOLS/share/mu4e \
  --maildir /SYNTHETIC/maildir --mail-manifest /SYNTHETIC/manifest.json \
  --workspace /RUNE_CHECKOUT --timeout 30 --output /tmp/doom-mu4e-status.json
```

Every gate starts a fresh process with an isolated HOME and explicit EMACSDIR,
DOOMDIR and DOOMLOCALDIR. Rune uses its **real bootstrap**, not raw-core mode.
Both runtimes must actually load the installed profile and enabled mu4e module.
The tested observable workflows are:
1. Real Doom profile/module boot.
2. Real sparse keymap definition/lookup and loaded Evil command availability.
3. Actual `.org` file: Org mode, heading TODO change, save, reopen, exact text.
4. Actual mu4e synthetic Maildir search/next/previous/body view/read-only checks,
   with source files unchanged. The passed-in driver is trusted executable test
   code and must itself be audited; this is not a sandbox.

JSON records versions (and whether mu/mu4e match), Doom/submodule/profile hashes,
binary hashes, upstream-driver hash, source-mail before/after hashes, per-gate
first errors, exit/timeout state and raw bounded logs. Passing requires exit 0,
an assertion marker emitted **after** real operations, matching versions and
unchanged source mail. Original temporary commands expire; rerun this script with
the same explicit inputs to regenerate Org fixtures. No upload or send occurs.

Batch boot does not prove native window/OS keyboard behavior or interactive
responsiveness. GNU successes are reference results only, never Rune results.

## Current measured status and next blockers

The qualified primitive checkpoint improved Rune **1/9 -> 3/9** after actual
bounded file import; GNU is **9/9**. Next primitive failure is read-only variable
forwarding. Separately, real full-Doom tests are **0/4 Rune, 0/4 GNU** while the
reference installer is incomplete (`doom-nosync-error`). Rune's latest separate
instrumented bootstrap diagnosis is `void-variable cl--loop-args`: file-local
special declarations were lost between freshly created interpreters. A rooted
per-load scope fix is in progress; rerun the actual benchmark after it lands.

The standalone GNU mu4e reference already passed real navigation, plain view and
read-only checks with immutable synthetic mail. That does **not** turn either
Doom or Rune green. End-to-end Doom+mu4e success by a next-day deadline remains
unestablished until these real gates execute successfully.
