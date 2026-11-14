#!/usr/bin/env python3
"""Actual pinned Doom boot + Org + upstream mu4e gates; no substitute viewer."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import tempfile
import uuid
from run_stdin_differential import literal
from run_upstream_ert import capture

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


def sources(marker, org_path, mu_driver, mu_home, mu_binary, mu_lisp):
    prelude = ('(setq noninteractive nil debug-on-error t mu4e-get-mail-command nil '
               'mu4e-update-interval nil mu4e-view-auto-mark-as-read nil '
               'mu4e-view-show-images nil mu4e-view-prefer-html nil '
               'shr-inhibit-images t gnus-inhibit-images t mu4e-mu-home ' + literal(str(mu_home)) +
               ' mu4e-mu-binary ' + literal(str(mu_binary)) + ')\n'
               '(add-to-list \'load-path ' + literal(str(mu_lisp)) + ')\n')
    boot = '''(unless (and (featurep 'doom) (bound-and-true-p doom-profile)) (error "Real Doom profile did not load"))
(unless (modulep! :email mu4e) (error "Doom mu4e module not enabled"))
'''
    keys = '''(let ((map (make-sparse-keymap)))
 (unless (keymapp map) (error "make-sparse-keymap did not create real keymap"))
 (define-key map (kbd "C-c t") #'ignore)
 (unless (eq (lookup-key map (kbd "C-c t")) #'ignore) (error "Real key binding lookup failed")))
(require 'evil)
(unless (fboundp 'evil-normal-state) (error "Doom Evil configuration unavailable"))
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
                     ('doom-mu4e-real-navigation', boot + '(load ' + literal(str(mu_driver)) + ' nil nil t)\n')]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('doom-root', 'doom-config', 'doom-local', 'gnu', 'rune', 'mu-driver', 'mu-home', 'mu-binary', 'mu-lisp', 'workspace', 'maildir', 'mail-manifest', 'output'):
        p.add_argument('--' + name, required=True, type=Path)
    p.add_argument('--revision', required=True)
    p.add_argument('--timeout', type=float, default=30)
    a = p.parse_args()
    import math
    if not math.isfinite(a.timeout) or not 0 < a.timeout <= 180:
        p.error('--timeout must be finite and in (0,180]')
    for name, path in vars(a).items():
        if isinstance(path, Path):
            setattr(a, name, path.resolve())
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
    marker = 'RUNE-DOOM-MU4E-' + uuid.uuid4().hex
    rows = []
    with tempfile.TemporaryDirectory(prefix='rune-doom-benchmark-') as temporary:
        temp = Path(temporary)
        prelude, gates = sources(marker, temp/'synthetic.org', a.mu_driver, a.mu_home, a.mu_binary, a.mu_lisp)
        (temp/'prelude.el').write_text(prelude, encoding='utf-8')
        for gate, form in gates:
            records = {}
            for runtime, executable in [('rune', a.rune), ('gnu', a.gnu)]:
                home = temp/(gate+'-'+runtime)
                home.mkdir()
                (temp/'synthetic.org').write_text(ORG, encoding='utf-8')
                script = temp/'assertions.el'
                script.write_text(form + '\n(prin1 (list ' + literal(marker) + ' \'passed))\n(setq noninteractive t)\n', encoding='utf-8')
                base = [sys.executable, '-c', LAUNCH, str(home), str(a.doom_config), str(a.doom_local), str(a.mu_home), str(a.mu_binary), str(a.doom_root), str(executable)]
                # Rune intentionally uses its REAL bootstrap; no no-bootstrap,
                # dummy provide, rewritten package, or miniature Lisp stand-in.
                if runtime == 'gnu':
                    command = base + ['-Q', '--batch', '--load', str(temp/'prelude.el'), '--load', str(a.doom_root/'early-init.el'), '--load', str(script)]
                else:
                    command = base + ['--load', str(temp/'prelude.el'), '--load', str(a.doom_root/'early-init.el'), '--load', str(script)]
                output, code, forced = capture(command, str(a.workspace), a.timeout)
                passed = forced is None and code == 0 and '(' + literal(marker) + ' passed)' in output
                errors = [line for line in output.splitlines() if re.search(r'Error:|error:|Debugger entered|void-function|file-missing|Cannot open', line)]
                records[runtime] = {'passed': passed and mail_hashes() == before_mail and versions_match, 'mail_sources_unchanged': mail_hashes() == before_mail, 'returncode': code, 'forced': forced, 'first_error': errors[0] if errors else None, 'output': output, 'command_template': command}
            rows.append({'id': gate, 'runtimes': records})
    import subprocess
    emacs_version, _, _ = capture([str(a.gnu), '--version'], str(a.workspace), 5)
    modules_commit = subprocess.check_output(['git','-C',str(a.doom_root/'sources/doom+'),'rev-parse','HEAD'], text=True).strip()
    commit = subprocess.check_output(['git','-C',str(a.doom_root),'rev-parse','HEAD'], text=True).strip()
    report = {'schema_version':1, 'measurement':'actual-doom-mu4e-synthetic-workflow', 'revision_label':a.revision,
              'doom_commit':commit, 'doom_modules_commit':modules_commit, 'gnu_emacs_version_output':emacs_version, 'mu4e_config_sha256':sha(a.mu_lisp/'mu4e-config.el'), 'mu_driver_sha256':sha(a.mu_driver), 'mail_manifest_sha256':sha(a.mail_manifest), 'mail_versions': {'mu':mu_version,'mu4e_source':mu4e_version,'matched':versions_match,'version_command_output':version_output}, 'mail_sources_before':before_mail,'mail_sources_after':mail_hashes(), 'doom_profile_sha256':sha(a.doom_config/'init.el'),
              'binary_sha256':{'rune':sha(a.rune),'gnu_launcher':sha(a.gnu),'mu_launcher':sha(a.mu_binary)},
              'gates':rows, 'summary':{runtime:{'passed':sum(r['runtimes'][runtime]['passed'] for r in rows),'total':len(rows),'first_failure':next(({'gate':r['id'],'error':r['runtimes'][runtime]['first_error']} for r in rows if not r['runtimes'][runtime]['passed']),None)} for runtime in ('rune','gnu')},
              'limitations':['Requires a genuinely installed pinned Doom profile; CLI help is not boot success.',
                 'GNU reference success is not Rune success. No feature mocks or viewer substitution.',
                 'Synthetic mu4e fixture/driver must disable retrieval, automatic read marking and image/network fetching.',
                 'This batch boot does not prove actual OS window/key dispatch or interactive responsiveness.',
                 'Temporary command paths expire; rerun this script with the same explicit inputs.']}
    a.output.parent.mkdir(parents=True,exist_ok=True)
    a.output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    return int(any(not r['runtimes']['rune']['passed'] or not r['runtimes']['gnu']['passed'] for r in rows))


if __name__=='__main__':
    raise SystemExit(main())
