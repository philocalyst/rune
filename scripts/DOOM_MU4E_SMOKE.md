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
Copy the official config/package examples, then apply `scripts/fixtures/doom-default-mu4e.patch` in that dedicated config directory. It enables bare mu4e and a documented synthetic context only (reserved test address, no SMTP or retrieval); it does not modify Doom or mu4e package source.
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

After installation/rebuild, capture the exact installed working-tree source baseline:

```
python3 scripts/package_provenance.py --repos /BENCH/local/straight/repos \
  --doom-root /DOOM --mu-lisp /TOOLS/share/mu4e --output /BENCH/package-source-manifest.json
```

The manifest is path-independent: repository/source-tree labels, exact HEAD revisions,
relative tracked-file content/type fingerprints (including initialized submodule
files), counts and bytes. It includes Doom's tracked sources, not just its commit.
The supplied mu4e Lisp source directory is also content-hashed as a plain tree
without inventing a Git revision. The benchmark requires an exact match before starting and checks sources and the
three profile files again afterward. Any mismatch invalidates all observations.
This is source provenance, not an atomic filesystem guarantee or a fingerprint of
untracked/compiled build caches. Rebuild caches from these recorded sources when
reproducing elsewhere. Hashing has package/file/byte caps; Git's tracked-name
metadata is materialized before file-count validation. Supplied repositories are
trusted benchmark dependencies, not arbitrary hostile inputs.

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
  --package-manifest /BENCH/package-source-manifest.json \
  --workspace /RUNE_CHECKOUT --timeout 300 --output /tmp/doom-mu4e-status.json
```

Every gate starts a fresh process with an isolated HOME and explicit EMACSDIR,
DOOMDIR and DOOMLOCALDIR. Rune uses its **real bootstrap**, not raw-core mode.
Both runtimes must actually load the installed profile, invoke official `doom-startup` (fully loads enabled modules/config) and `doom-finalize`, then exercise the enabled mu4e module. Loading a profile descriptor alone is insufficient. Success/failure uses the existing `message` primitive; no extra `prin1` dependency or fake feature is introduced. Failure is reported and re-signalled, never caught and counted as success.
The tested observable workflows are:
1. Real Doom profile/module boot.
2. Real sparse keymap definition/lookup and actual local Evil escape key binding dispatched through `call-interactively`, restoring normal state.
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

These are separate checkpoints, not interchangeable scores:
- Raw-core prerequisite smoke improved Rune **1/9 -> 3/9 -> 4/9** with actual
  bounded file import and owner-enforced read-only state; GNU is **9/9**.
  Raw stdin bypasses bootstrap. Its source-mirrored package `defmacro` failure
  is NOT evidence that bootstrapped Rune lacks that macro.
- Official Doom installation and rebuild completed exit 0. This alone proves no
  workflow. Corrected **real `doom-startup`** reference tests now pass GNU **4/4**:
  profile startup, actual Evil dispatch, Org edit/save/reopen, and actual mu4e
  navigation/body/read-only. The profile uses Doom's documented synthetic context,
  with no credentials, retrieval or outgoing mail. Source mail hashes stayed unchanged.
- Earlier descriptor-only GNU "boot" passes were explicitly withdrawn and old
  JSON reports annotated. They are not full startup evidence.
- Rune's real bootstrap passes with faithful native minibuffer-map initialization.
  The actual Doom first gate remains **0/1**. After the working-directory fix
  cleared the empty-path error, official `early-init.el` reports
  `(void-function getenv-internal)`. This is a real package prerequisite, not
  an installer pass or a raw-core macro failure. Later workflows are not run
  until that genuine first gate progresses.
- Standalone **actual GNU mu4e** navigation/body/read-only tests passed, with all
  synthetic mail unchanged. This is a reference only, never Rune or Doom success.

Run a single first-failure stage with `--runtime rune --gate doom-profile-boot
--timeout 300` before spending four full bootstraps. Default timeout is 300s
(maximum 600s); the observed 128s real bootstrap cannot be tested honestly with a
short timeout. A runtime excluded by `--runtime` is `not-run`, not a failed run.

Fresh reports and named conditions drive the next fix. End-to-end Doom+mu4e
success by a next-day deadline remains unestablished until the actual gates pass.
Batch results still do not establish native OS key/window behavior.
