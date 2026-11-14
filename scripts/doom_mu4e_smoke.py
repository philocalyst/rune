#!/usr/bin/env python3
"""Actual pinned Doom boot + Org + upstream mu4e gates; no substitute viewer."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import subprocess
import stat
import tempfile
import uuid
from run_stdin_differential import literal
from run_upstream_ert import capture
from package_provenance import capture as capture_package_sources

LAUNCH = '''import os,sys
home,config,local,mu_home,mu_binary,doom_root=sys.argv[1:7]
os.environ.update(HOME=home,XDG_CONFIG_HOME=home+'/config',XDG_CACHE_HOME=home+'/cache',XDG_DATA_HOME=home+'/data',XDG_STATE_HOME=home+'/state',EMACSDIR=doom_root,DOOMDIR=config,DOOMLOCALDIR=local,MAIL='',MAILDIR='')
os.execv(sys.argv[7],sys.argv[7:])
'''
ORG = '#+title: Synthetic Doom benchmark\n* TODO First λ task\nSynthetic body.\n* TODO Second task\n'


def sha(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def preload_fingerprints(paths):
    """Content/order identity only; local paths are recorded separately."""
    if len(paths) > 8:
        raise ValueError('At most eight explicit Rune source preloads are supported')
    rows = []
    for index, path in enumerate(paths):
        info = path.stat()
        if not stat.S_ISREG(info.st_mode) or info.st_size > 8 * 1024 * 1024:
            raise ValueError('Rune preloads must be regular source files of at most 8 MiB')
        digest = hashlib.sha256()
        consumed = 0
        with path.open('rb') as stream:
            for chunk in iter(lambda:stream.read(1024 * 1024), b''):
                consumed += len(chunk)
                if consumed > 8 * 1024 * 1024:
                    raise ValueError('Rune preload grew beyond 8 MiB while hashing')
                digest.update(chunk)
        rows.append({'order':index, 'name':path.name, 'sha256':digest.hexdigest()})
    return rows


def preload_wrapper(paths, marker, failure_handler):
    # Source bodies remain untouched. This wrapper reports failures and does not
    # invoke GNU CLI/UI entry or invent any missing definitions/features.
    # NOSUFFIX pins the actual loaded bytes to the exact fingerprinted path.
    # This seam is for the now-qualified load-flags runtime, not older cores.
    loads = ''.join('(load ' + literal(str(path)) + ' nil nil t)\n' for path in paths)
    return ('(condition-case rune-benchmark-error (progn\n' + loads +
            '(let ((inhibit-message nil)) (message ' + literal(marker+':preloads-complete') +
            ')))\n' + failure_handler + ')\n')


def sources(marker, org_path, mu_driver, mu_home, mu_binary, mu_lisp):
    prelude = ('(setq noninteractive nil debug-on-error t mu4e-get-mail-command nil '
               'mu4e-update-interval nil mu4e-view-auto-mark-as-read nil '
               'mu4e-view-show-images nil mu4e-view-prefer-html nil '
               'shr-inhibit-images t gnus-inhibit-images t mu4e-mu-home ' + literal(str(mu_home)) +
               ' mu4e-mu-binary ' + literal(str(mu_binary)) + ')\n'
               '(add-to-list \'load-path ' + literal(str(mu_lisp)) + ')\n')
    boot = '''(doom-startup)
(doom-finalize)
(unless (and (featurep 'doom) (bound-and-true-p doom-profile)) (error "Real Doom profile did not load"))
(unless (modulep! :email mu4e) (error "Doom mu4e module not enabled"))
'''
    keys = '''(let ((map (make-sparse-keymap)))
 (unless (keymapp map) (error "make-sparse-keymap did not create real keymap"))
 (define-key map (kbd "C-c t") #'ignore)
 (unless (eq (lookup-key map (kbd "C-c t")) #'ignore) (error "Real key binding lookup failed")))
(require 'evil)
(unless (fboundp 'evil-normal-state) (error "Doom Evil configuration unavailable"))
(with-temp-buffer
 (evil-local-mode 1)
 (evil-insert-state)
 (let ((command (key-binding [escape])))
  (unless (commandp command) (error "Real local Evil escape binding unavailable"))
  (call-interactively command))
 (unless (eq evil-state 'normal) (error "Actual Evil escape dispatch did not restore normal state")))
'''
    org = '''(find-file ''' + literal(str(org_path)) + ''')
(org-mode)
(setq org-log-done nil)
(goto-char (point-min))
(re-search-forward "^\\\\* TODO ")
(beginning-of-line)
(org-todo "DONE")
(unless (equal (org-get-todo-state) "DONE") (error "Org TODO change failed"))
(save-buffer)
(let ((expected (buffer-string)))
 (revert-buffer t t)
 (unless (equal expected (buffer-string)) (error "Org save/reopen changed text")))
'''
    return prelude, [('doom-profile-boot', boot), ('doom-real-keymap', boot + keys),
                     ('doom-org-edit-save-reopen', boot + org),
                     ('doom-mu4e-real-navigation', boot + '(load ' + literal(str(mu_driver)) + ' nil nil)\n')]


def summarize(rows):
    result = {}
    for runtime in ('rune', 'gnu'):
        executed = [row for row in rows if row['runtimes'][runtime].get('status') != 'not-run']
        first = next((row for row in executed if not row['runtimes'][runtime]['passed']), None)
        result[runtime] = {'passed': sum(row['runtimes'][runtime]['passed'] for row in executed),
                           'total': len(rows), 'executed': len(executed),
                           'not_run': len(rows)-len(executed),
                           'first_failure': None if first is None else {'gate':first['id'],'error':first['runtimes'][runtime]['first_error']}}
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('doom-root', 'doom-config', 'doom-local', 'gnu', 'rune', 'mu-driver', 'mu-home', 'mu-binary', 'mu-lisp', 'workspace', 'maildir', 'mail-manifest', 'package-manifest', 'output'):
        p.add_argument('--' + name, required=True, type=Path)
    p.add_argument('--rune-preload', action='append', type=Path, default=[],
                   help='Explicit genuine source file loaded on Rune only before benchmark prelude; repeat in dependency order')
    p.add_argument('--revision', required=True)
    p.add_argument('--timeout', type=float, default=300)
    p.add_argument('--runtime', action='append', choices=['rune','gnu'], default=[])
    p.add_argument('--gate', action='append', choices=['doom-profile-boot','doom-real-keymap','doom-org-edit-save-reopen','doom-mu4e-real-navigation'], default=[])
    a = p.parse_args()
    import math
    if not math.isfinite(a.timeout) or not 0 < a.timeout <= 600:
        p.error('--timeout must be finite and in (0,600]')
    for name, path in vars(a).items():
        if isinstance(path, Path):
            setattr(a, name, path.resolve())
    if len(a.rune_preload) > 8:
        p.error('At most eight explicit Rune source preloads are supported')
    a.rune_preload = [path.resolve() for path in a.rune_preload]
    try:
        rune_preloads_before = preload_fingerprints(a.rune_preload)
    except (OSError, ValueError) as error:
        p.error(str(error))
    expected_packages = json.loads(a.package_manifest.read_text(encoding='utf-8'))
    package_sources_before = capture_package_sources(a.doom_local/'straight/repos', {'@doom':a.doom_root}, {'@mu4e':a.mu_lisp})
    if package_sources_before != expected_packages:
        p.error('installed package revisions/sources do not match supplied manifest')
    manifest = json.loads(a.mail_manifest.read_text(encoding='utf-8'))
    expected_mail = manifest.get('synthetic_message_sha256', {})
    if not expected_mail or len(expected_mail) > 100 or any(Path(name).is_absolute() or '..' in Path(name).parts for name in expected_mail):
        p.error('expected a bounded synthetic message manifest with relative paths')
    def mail_hashes():
        return {name: sha(a.maildir/name) if (a.maildir/name).is_file() else None for name in expected_mail}
    before_mail = mail_hashes()
    if before_mail != expected_mail:
        p.error('synthetic source bytes do not match supplied manifest; refusing benchmark')
    version_output, version_code, _ = capture([str(a.mu_binary), '--version'], str(a.workspace), 5)
    mu_match = re.search(r'version ([0-9.]+)', version_output)
    mu4e_match = re.search(r'\(defconst mu4e-mu-version "([^"]+)"', (a.mu_lisp/'mu4e-config.el').read_text(encoding='utf-8'))
    mu_version = mu_match[1] if mu_match else None
    mu4e_version = mu4e_match[1] if mu4e_match else None
    versions_match = version_code == 0 and mu_version is not None and mu_version == mu4e_version
    profile_sources_before = {name:sha(a.doom_config/name) for name in ('init.el','config.el','packages.el')}
    fixture_paths = {'mu4e_config':a.mu_lisp/'mu4e-config.el', 'mu_driver':a.mu_driver, 'mail_manifest':a.mail_manifest, 'package_manifest':a.package_manifest}
    fixture_sources_before = {name:sha(path) for name,path in fixture_paths.items()}
    modules_commit = subprocess.check_output(['git','-C',str(a.doom_root/'sources/doom+'),'rev-parse','HEAD'], text=True).strip()
    commit = package_sources_before['packages']['@doom']['revision']
    binaries_before = {'rune':sha(a.rune),'gnu_launcher':sha(a.gnu),'mu_launcher':sha(a.mu_binary)}
    marker = 'RUNE-DOOM-MU4E-' + uuid.uuid4().hex
    rows = []
    with tempfile.TemporaryDirectory(prefix='rune-doom-benchmark-') as temporary:
        temp = Path(temporary)
        prelude, gates = sources(marker, temp/'synthetic.org', a.mu_driver, a.mu_home, a.mu_binary, a.mu_lisp)
        (temp/'prelude.el').write_text(prelude, encoding='utf-8')
        failure_handler = '(t (let ((inhibit-message nil)) (message ' + literal(marker + ':failure %S') + ' rune-benchmark-error)) (signal (car rune-benchmark-error) (cdr rune-benchmark-error)))'
        if a.rune_preload:
            (temp/'rune-preloads.el').write_text(preload_wrapper(a.rune_preload,marker,failure_handler), encoding='utf-8')
        (temp/'early-wrapper.el').write_text('(condition-case rune-benchmark-error (load ' + literal(str(a.doom_root/'early-init.el')) + ' nil nil) ' + failure_handler + ')\n', encoding='utf-8')
        for gate, form in gates:
            if a.gate and gate not in a.gate:
                continue
            records = {}
            for runtime, executable in [('rune', a.rune), ('gnu', a.gnu)]:
                if a.runtime and runtime not in a.runtime:
                    records[runtime] = {'passed':False,'status':'not-run','first_error':None}
                    continue
                home = temp/(gate+'-'+runtime)
                home.mkdir()
                (temp/'synthetic.org').write_text(ORG, encoding='utf-8')
                script = temp/'assertions.el'
                script.write_text('(condition-case rune-benchmark-error (progn\n' + form + '\n(let ((inhibit-message nil)) (message ' + literal(marker + ':passed') + ')))\n' + failure_handler + ')\n(setq noninteractive t)\n', encoding='utf-8')
                base = [sys.executable, '-c', LAUNCH, str(home), str(a.doom_config), str(a.doom_local), str(a.mu_home), str(a.mu_binary), str(a.doom_root), str(executable)]
                # Rune intentionally uses its REAL bootstrap; no no-bootstrap,
                # dummy provide, rewritten package, or miniature Lisp stand-in.
                if runtime == 'gnu':
                    command = base + ['-Q', '--batch', '--load', str(temp/'prelude.el'), '--load', str(temp/'early-wrapper.el'), '--load', str(script)]
                else:
                    preload_command = ['--load', str(temp/'rune-preloads.el')] if a.rune_preload else []
                    command = base + preload_command + ['--load', str(temp/'prelude.el'), '--load', str(temp/'early-wrapper.el'), '--load', str(script)]
                output, code, forced = capture(command, str(a.workspace), a.timeout)
                preloads_required = runtime == 'rune' and bool(a.rune_preload)
                preloads_completed = preloads_required and any(line in (marker+':preloads-complete','MESSAGE: '+marker+':preloads-complete') for line in output.splitlines())
                passed = (forced is None and code == 0
                          and any(line in (marker+':passed', 'MESSAGE: '+marker+':passed') for line in output.splitlines())
                          and (not preloads_required or preloads_completed))
                structured = [line for line in output.splitlines() if line.startswith(marker+':failure ') or line.startswith('MESSAGE: '+marker+':failure ')]
                errors = structured or [line for line in output.splitlines() if re.search(r'Error:|error:|Debugger entered|void-function|file-missing|Cannot open', line)]
                if not errors and forced is None and code == 0 and preloads_required and not preloads_completed:
                    errors = ['Required Rune source preload completion marker missing']
                records[runtime] = {'passed': passed and mail_hashes() == before_mail and versions_match, 'mail_sources_unchanged': mail_hashes() == before_mail, 'returncode': code, 'forced': forced, 'first_error': errors[0] if errors else None, 'output': output, 'command_template': command, 'rune_preloads_required': preloads_required, 'rune_preloads_completed': preloads_completed}
            rows.append({'id': gate, 'runtimes': records})
    emacs_version, _, _ = capture([str(a.gnu), '--version'], str(a.workspace), 5)
    try:
        binaries_after = {'rune':sha(a.rune),'gnu_launcher':sha(a.gnu),'mu_launcher':sha(a.mu_binary)}
    except Exception as error:
        binaries_after = {'capture_error':str(error)[:512]}
    try:
        fixture_sources_after = {name:sha(path) for name,path in fixture_paths.items()}
    except Exception as error:
        fixture_sources_after = {'capture_error':str(error)[:512]}
    try:
        package_sources_after = capture_package_sources(a.doom_local/'straight/repos', {'@doom':a.doom_root}, {'@mu4e':a.mu_lisp})
    except Exception as error:
        # Preserve executed observations as explicitly invalid, even if a source
        # was deleted/replaced rather than merely changed during the workflow.
        package_sources_after = {'capture_error':str(error)[:512]}
    try:
        profile_sources_after = {name:sha(a.doom_config/name) for name in ('init.el','config.el','packages.el')}
    except Exception as error:
        profile_sources_after = {'capture_error':str(error)[:512]}
    profile_sources_unchanged = profile_sources_before == profile_sources_after
    try:
        rune_preloads_after = preload_fingerprints(a.rune_preload)
    except (OSError, ValueError) as error:
        rune_preloads_after = {'capture_error':str(error)[:512]}
    rune_preloads_unchanged = rune_preloads_before == rune_preloads_after
    if not rune_preloads_unchanged:
        for row in rows:
            if row['runtimes']['rune'].get('status') != 'not-run':
                row['runtimes']['rune']['passed'] = False
                row['runtimes']['rune']['first_error'] = 'Explicit Rune source preload changed during benchmark; observations unqualified'
    package_sources_unchanged = package_sources_before == package_sources_after
    if binaries_before != binaries_after or not package_sources_unchanged or not profile_sources_unchanged or fixture_sources_before != fixture_sources_after:
        for row in rows:
            for runtime in ('rune','gnu'):
                if row['runtimes'][runtime].get('status') != 'not-run':
                    row['runtimes'][runtime]['passed'] = False
                    row['runtimes'][runtime]['first_error'] = 'Executable or installed package source changed during benchmark; observations unqualified'
    report = {'schema_version':2, 'startup_contract':'official doom-startup and doom-finalize before all assertions', 'measurement':'actual-doom-mu4e-synthetic-workflow', 'revision_label':a.revision,
              'rune_preload_paths': [str(path) for path in a.rune_preload], 'rune_preloads_before':rune_preloads_before, 'rune_preloads_after':rune_preloads_after, 'rune_preloads_unchanged':rune_preloads_unchanged, 'rune_startup_path':'normal bootstrap plus explicit genuine source preloads' if a.rune_preload else 'normal bootstrap only', 'fixture_sources_before':fixture_sources_before, 'fixture_sources_after':fixture_sources_after, 'fixture_sources_unchanged':fixture_sources_before==fixture_sources_after, 'profile_sources_before':profile_sources_before, 'profile_sources_after':profile_sources_after, 'profile_sources_unchanged':profile_sources_unchanged, 'package_manifest_sha256':fixture_sources_before['package_manifest'], 'package_sources_before':package_sources_before, 'package_sources_after':package_sources_after, 'package_sources_unchanged':package_sources_unchanged, 'doom_commit':commit, 'doom_modules_commit':modules_commit, 'gnu_emacs_version_output':emacs_version, 'mu4e_config_sha256':fixture_sources_before['mu4e_config'], 'mu_driver_sha256':fixture_sources_before['mu_driver'], 'mail_manifest_sha256':fixture_sources_before['mail_manifest'], 'mail_versions': {'mu':mu_version,'mu4e_source':mu4e_version,'matched':versions_match,'version_command_output':version_output}, 'mail_sources_before':before_mail,'mail_sources_after':mail_hashes(), 'doom_profile_sha256':profile_sources_before['init.el'], 'doom_config_sha256':profile_sources_before['config.el'], 'doom_packages_sha256':profile_sources_before['packages.el'],
              'binary_sha256':binaries_before, 'binary_sha256_after':binaries_after, 'executables_unchanged':binaries_before==binaries_after,
              'gates':rows, 'summary':summarize(rows),
              'limitations':['Requires a genuinely installed pinned Doom profile; CLI help is not boot success.',
                 'GNU reference success is not Rune success. No feature mocks or viewer substitution.',
                 'Synthetic mu4e fixture/driver must disable retrieval, automatic read marking and image/network fetching.',
                 'This batch boot does not prove actual OS window/key dispatch or interactive responsiveness.',
                 'Temporary command paths expire; rerun this script with the same explicit inputs.', 'first_error is a heuristic log line; raw output and structured diagnostics are authoritative.']}
    a.output.parent.mkdir(parents=True,exist_ok=True)
    a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    return int(any(not r['runtimes'][runtime]['passed'] for r in rows for runtime in (a.runtime or ['rune','gnu'])))


if __name__=='__main__':
    raise SystemExit(main())
