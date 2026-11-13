#!/usr/bin/env python3
"""Compare a single-line seed batch via supported Rune stdin and GNU batch APIs."""
import argparse
import json
import math
from pathlib import Path
import re
import sys
import tempfile
import uuid

from run_upstream_ert import capture

LAUNCHER = '''import subprocess,sys
with open(sys.argv[1], 'rb') as stream:
    raise SystemExit(subprocess.call(sys.argv[2:], stdin=stream))
'''


def literal(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t') + '"'


def read_cases(path):
    data = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(data, list) or not 1 <= len(data) <= 5000:
        raise ValueError('expected a JSON list of 1 to 5000 seed cases')
    seen = set()
    for case in data:
        if not isinstance(case, dict) or set(case) != {'id', 'form'} or any(
                not isinstance(case[k], str) or not case[k].strip() for k in ('id', 'form')):
            raise ValueError('each case needs only nonempty id and form strings')
        if any(char in case['form'] for char in ('\n', '\r', ';', '\x00')):
            raise ValueError('bridge requires single-line, semicolon-free form bodies')
        if case['id'] in seen:
            raise ValueError('duplicate case id: ' + case['id'])
        seen.add(case['id'])
    return data


def driver(cases, marker, flavor):
    forms = []
    for index, case in enumerate(cases):
        # Direct forms, not Lisp read/eval. Rune's Rust CLI prints the returned
        # list after evaluation, avoiding unavailable Lisp princ/prin1 as well.
        expression = ('(list ' + literal(marker) + ' ' + str(index) +
                      ' (condition-case err (list \'value (progn ' + case['form'] +
                      ")) (error (list 'error (car err) (cdr err)))))")
        if flavor == 'emacs':
            expression = ('(let ((rune-diff-observation ' + expression + ')) '
                          '(princ "\\n") '
                          '(let ((print-length nil) (print-level nil) (print-circle t) '
                          '(print-escape-newlines t)) (prin1 rune-diff-observation)) (terpri))')
        forms.append(expression)
    end = '(list ' + literal(marker) + " 'done " + str(len(cases)) + ')'
    forms.append('(progn (prin1 ' + end + ') (terpri))' if flavor == 'emacs' else end)
    return '\n'.join(forms) + '\n'


def parse(output, marker, count):
    prefix = '(' + literal(marker) + ' '
    pattern = re.compile(re.escape(prefix) + r'([0-9]+) (\((?:value|error) .+\))\)$')
    results, done = {}, False
    for line in output.splitlines():
        if not line.startswith(prefix):
            continue
        if line == prefix + 'done ' + str(count) + ')':
            if done:
                raise ValueError('duplicate completion')
            done = True
            continue
        match = pattern.fullmatch(line)
        if not match or done:
            raise ValueError('invalid observation framing')
        index = int(match[1])
        if str(index) != match[1] or index != len(results) or index >= count:
            raise ValueError('nonsequential or duplicate observation')
        results[index] = match[2]
    return results, done


def run_batch(cases, command, flavor, timeout, cwd=None):
    marker = 'RUNE-STDIN-DIFF-' + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='rune-stdin-diff-') as tmp:
        script = Path(tmp) / 'batch.el'
        script.write_text(driver(cases, marker, flavor), encoding='utf-8')
        if flavor == 'rune':
            args = [sys.executable, '-c', LAUNCHER, str(script), *command, '--eval-stdin']
        else:
            args = [*command, '-Q', '--batch', '--load', str(script)]
        output, code, forced = capture(args, cwd, timeout)
    try:
        results, done = parse(output, marker, len(cases))
        status = forced or ('completed' if code == 0 and done and len(results) == len(cases) else 'incomplete')
    except ValueError as exc:
        results, status = {}, forced or 'protocol-error'
        output += '\n' + str(exc)
    return {'status': status, 'returncode': code, 'results': results, 'output': output}


def compare(cases, rune, oracle):
    rows = []
    for index, case in enumerate(cases):
        left, right = rune['results'].get(index), oracle['results'].get(index)
        rows.append(dict(case, rune=left, oracle=right,
                         status='not-observed' if left is None or right is None else
                         'matched' if left == right else 'mismatched'))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('cases', type=Path)
    parser.add_argument('--rune', default='./target/release/rune')
    parser.add_argument('--rune-arg', action='append', default=[])
    parser.add_argument('--oracle', default='emacs')
    parser.add_argument('--oracle-arg', action='append', default=[])
    parser.add_argument('--timeout', type=float, default=60)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--oracle-revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('--timeout must be finite and positive')
    try:
        cases = read_cases(args.cases)
    except (OSError, ValueError) as exc:
        parser.error(str(exc))
    rune = run_batch(cases, [args.rune, *args.rune_arg], 'rune', args.timeout)
    oracle = run_batch(cases, [args.oracle, *args.oracle_arg], 'emacs', args.timeout)
    rows = compare(cases, rune, oracle)
    data = {'schema_version': 1, 'measurement': 'stdin-exact-printer-differential',
            'revision': args.revision, 'oracle_revision': args.oracle_revision,
            'limitations': ['Exact printer text, not structural normalization or state comparison.',
                            'Raw-core Rune stdin bypasses bootstrap; GNU -Q has GNU startup environment.',
                            'Single-line semicolon-free trusted forms, shared batch state, no shrinking.',
                            'Rune print flags are not implemented; multiline/circular/noisy results can fail transport.'],
            'runtimes': {'rune': rune, 'oracle': oracle}, 'cases': rows}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')
    return int(any(run['status'] != 'completed' for run in (rune, oracle)) or
               any(row['status'] != 'matched' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
