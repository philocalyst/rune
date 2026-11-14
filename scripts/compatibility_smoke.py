#!/usr/bin/env python3
"""Synthetic file-backed Org/read-only-mail vertical-slice gates, not a sandbox."""
import argparse
import hashlib
import json
from pathlib import Path
import sys
import tempfile

from run_stdin_differential import literal, run_batch

ORG = '#+title: Synthetic roadmap\n* TODO Review λ release\nBody with [[https://example.invalid][fake link]].\n** DONE Draft notes\n'
MAIL = ('From sender@example.invalid Sat Nov 14 10:00:00 2026\n'
        'From: Synthetic Sender <sender@example.invalid>\nTo: reader@example.invalid\n'
        'Subject: Synthetic release λ\nDate: Sat, 14 Nov 2026 10:00:00 +0000\n'
        'Message-ID: <one@example.invalid>\nMIME-Version: 1.0\nContent-Type: text/plain\n\n'
        'Synthetic message one. No real user data.\n\n'
        'From second@example.invalid Sat Nov 14 11:00:00 2026\n'
        'From: Second Synthetic <second@example.invalid>\nTo: reader@example.invalid\n'
        'Subject: Synthetic second message\nMessage-ID: <two@example.invalid>\n\n'
        'Synthetic message two.\n')
# Keep user HOME/config/mail out of both runtime subprocesses. No network/mail
# retrieval/send functions occur in generated forms; only synthetic files open.
ISOLATE = ('import os,sys; os.environ.update(HOME=sys.argv[1],'
           'XDG_CONFIG_HOME=sys.argv[1],MAIL="",MAILDIR=""); '
           'os.execv(sys.argv[2],sys.argv[2:])')


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def fixtures(directory):
    result = {}
    for key, name, text in [('org', 'synthetic.org', ORG), ('mail', 'synthetic.mbox', MAIL)]:
        path = directory / name
        path.write_text(text, encoding='utf-8')
        result[key] = path
    return result


def gates(paths, lisp_dirs=()):
    setup = '(set-buffer (get-buffer-create "*synthetic-compat*")) '
    def load(key):
        return setup + '(insert-file-contents ' + literal(str(paths[key])) + ') '
    def exact(text):
        return '(equal (buffer-substring-no-properties (point-min) (point-max)) ' + literal(text) + ')'
    prefix = ''
    if lisp_dirs:
        prefix = '(setq load-path (append (list ' + ' '.join(literal(str(p)) for p in lisp_dirs) + ') load-path)) '
    return [
        ('core-text', 'Unicode owner buffer insert/read prerequisite',
         setup + '(insert ' + literal(ORG) + ') ' + exact(ORG)),
        ('org-file-read', 'Read actual synthetic .org file without conversion loss', load('org') + exact(ORG)),
        ('mailbox-file-read', 'Read actual two-message synthetic mbox without conversion loss', load('mail') + exact(MAIL)),
        ('mail-read-only', 'Browse imported mailbox; reject edits and preserve content', load('mail') +
         '(setq buffer-read-only t) (goto-char (point-min)) '
         '(and (eq (char-after) 70) (condition-case nil (progn (insert "FORBIDDEN") nil) (buffer-read-only t)) ' + exact(MAIL) + ')'),
        ('org-package-load', 'Load real Org library, independent of file-read prerequisite', prefix + "(require 'org) (featurep 'org)"),
        ('mail-package-load', 'Load real Rmail library, independent of file-read prerequisite', prefix + "(require 'rmail) (featurep 'rmail)"),
        ('org-mode', 'Load real Org and initialize mode in file-backed synthetic buffer', prefix + load('org') +
         '(require \'org) (org-mode) (eq major-mode \'org-mode)'),
        ('mail-header-browse', 'Load mail-utils and extract synthetic message subject', prefix + load('mail') +
         '(require \'mail-utils) (goto-char (point-min)) (forward-line 1) '
         '(equal (mail-fetch-field "Subject") "Synthetic release λ")'),
        ('rmail-mode', 'Load real Rmail and initialize read-only synthetic mailbox view', prefix + load('mail') +
         '(require \'rmail) (rmail-mode) (and (eq major-mode \'rmail-mode) buffer-read-only)'),
    ]


def execute(rune, oracle, revision, oracle_revision, timeout, directory, lisp_dirs=(), runner=run_batch, selected=()):
    paths = fixtures(directory)
    before = {key: digest(path) for key, path in paths.items()}
    home = directory / 'isolated-home'
    home.mkdir()
    records = []
    for gate, requirement, form in gates(paths, lisp_dirs):
        if selected and gate not in selected:
            continue
        observations = {}
        for flavor, executable in [('rune', rune), ('emacs', oracle)]:
            if executable is None:
                observations[flavor] = {'status': 'not-run', 'passed': False, 'reason': 'executable not supplied'}
                continue
            command = [sys.executable, '-c', ISOLATE, str(home), str(Path(executable).resolve())]
            run = runner([{'id': gate, 'form': form}], command, flavor, timeout, str(directory))
            actual = run['results'].get(0)
            observations[flavor] = dict(run, passed=run['status'] == 'completed' and actual == '(value t)', actual=actual)
        records.append({'id': gate, 'requirement': requirement, 'form': form, 'runtimes': observations})
    unchanged = {key: before[key] == digest(path) for key, path in paths.items()}
    first = next((r for r in records if not r['runtimes']['rune']['passed']), None)
    summary = {flavor: {'passed': sum(row['runtimes'][flavor]['passed'] for row in records), 'total': len(records), 'first_failure': next(({'gate': row['id'], 'status': row['runtimes'][flavor]['status'], 'actual': row['runtimes'][flavor].get('actual')} for row in records if not row['runtimes'][flavor]['passed']), None)} for flavor in ('rune', 'emacs')}
    return {'schema_version': 1, 'measurement': 'synthetic-org-mail-vertical-slice',
            'overall_status': 'blocked' if first else 'measured-gates-pass-unmeasured-workflows', 'summary': summary, 'revision_label': revision, 'oracle_revision_label': oracle_revision,
            'binary_sha256': {'rune': digest(Path(rune)), 'oracle_launcher': digest(Path(oracle)) if oracle else None},
            'fixture_sha256': before, 'fixtures_unchanged': unchanged,
            'first_rune_failure': None if first is None else {'id': first['id'], 'form': first['form'], 'observation': first['runtimes']['rune']},
            'gates': records,
            'unmeasured_requirements': ['Org heading navigation, TODO editing and exact save/reopen roundtrip',
                'Rmail two-message parsing/navigation and MIME display without writing mailbox',
                'Actual native window, keyboard/IME and render interaction'],
            'limitations': ['Raw Rune stdin bypasses bootstrap; GNU -Q startup differs.',
                'Mode initialization is not complete Org/Rmail workflow coverage.',
                'Synthetic fixtures only; no account, network retrieval, send or user mailbox access.',
                'HOME is isolated for configuration hygiene, not a filesystem/network sandbox.',
                'Revision labels are caller assertions; binary bytes are SHA256 recorded.']}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rune', required=True, type=Path)
    p.add_argument('--oracle', type=Path)
    p.add_argument('--revision', required=True)
    p.add_argument('--oracle-revision', default='not-supplied')
    p.add_argument('--timeout', type=float, default=10)
    p.add_argument('--lisp-dir', action='append', type=Path, default=[])
    p.add_argument('--gate', action='append', default=[], help='run only named gate(s), regenerating synthetic fixtures')
    p.add_argument('--output', required=True, type=Path)
    args = p.parse_args()
    import math
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 120:
        p.error('--timeout must be finite and in (0,120] seconds per gate/runtime')
    valid = {gate for gate, _, _ in gates({'org': Path('/synthetic.org'), 'mail': Path('/synthetic.mbox')})}
    if set(args.gate) - valid:
        p.error('unknown gate: ' + ', '.join(sorted(set(args.gate) - valid)))
    with tempfile.TemporaryDirectory(prefix='rune-synthetic-compat-') as tmp:
        report = execute(args.rune, args.oracle, args.revision, args.oracle_revision, args.timeout, Path(tmp), args.lisp_dir, selected=args.gate)
    report['reproduce'] = {'command': [sys.executable, str(Path(__file__).resolve()), '--rune', str(args.rune.resolve()), '--revision', args.revision, '--timeout', str(args.timeout), '--output', '/tmp/rune-compat-reproduction.json'], 'first_gate': report['first_rune_failure']['id'] if report['first_rune_failure'] else None}
    if args.oracle:
        report['reproduce']['command'] += ['--oracle', str(args.oracle.resolve()), '--oracle-revision', args.oracle_revision]
    for directory in args.lisp_dir:
        report['reproduce']['command'] += ['--lisp-dir', str(directory.resolve())]
    if report['first_rune_failure']:
        report['reproduce']['command'] += ['--gate', report['first_rune_failure']['id']]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
    return int(not all(report['fixtures_unchanged'].values()) or any(not g['runtimes']['rune']['passed'] for g in report['gates']))


if __name__ == '__main__':
    raise SystemExit(main())
