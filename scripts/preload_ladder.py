#!/usr/bin/env python3
"""Measure an explicit ordered preload manifest via Rune's --eval-stdin CLI."""
import argparse
import json
import math
from pathlib import Path
import re
import sys
import tempfile
import uuid

from run_upstream_ert import capture

# Keep the optional transport wrapper in the same POSIX process group as the
# runtime. A file-backed stdin avoids pipe deadlocks for large manifests.
LAUNCHER = '''import subprocess,sys
with open(sys.argv[1], 'rb') as stream:
    raise SystemExit(subprocess.call(sys.argv[2:], stdin=stream))
'''


def literal(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t') + '"'


def read_manifest(path, lisp_root):
    files = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(files, list) or not 1 <= len(files) <= 5000:
        raise ValueError('manifest must be a list of 1 to 5000 relative .el file names')
    root = lisp_root.resolve()
    if not root.is_dir():
        raise ValueError('Lisp source root must be an existing directory')
    seen, rows = set(), []
    for name in files:
        if not isinstance(name, str) or not name or '\x00' in name or Path(name).is_absolute() or Path(name).suffix != '.el':
            raise ValueError('manifest entries must be relative .el file names')
        target = (root / name).resolve()
        if not target.is_relative_to(root):
            raise ValueError('manifest entry escapes Lisp source root: ' + name)
        if target in seen:
            raise ValueError('duplicate manifest source: ' + name)
        seen.add(target)
        # A missing source is allowed to become the measured first blocker.
        rows.append({'file': target.relative_to(root).as_posix(), 'path': str(target)})
    return rows


def driver(files, marker):
    source_root = Path(files[0]['path'])
    for _ in Path(files[0]['file']).parts:
        source_root = source_root.parent
    lines = ['(setq load-path (list ' + literal(str(source_root)) + '))',
             '(setq rune-preload-running t)']
    for index, row in enumerate(files):
        # Return simple marker lists. Printing is performed by Rust's CLI, not
        # unavailable Lisp princ/prin1, and forms need no Lisp read/eval either.
        record = lambda status: '(list ' + literal(marker) + ' ' + str(index) + " '" + status + ')'
        lines.append('(if rune-preload-running\n'
                     '    (condition-case err\n'
                     '        (progn (load ' + literal(row['path']) + ' nil t) ' + record('loaded') + ')\n'
                     '      (error (setq rune-preload-running nil) ' + record('load-error') + '))\n'
                     '  ' + record('not-attempted') + ')')
    lines.append('(list ' + literal(marker) + " 'done " + str(len(files)) + ')')
    # The current CLI reads one physical line before attempting a form. Keep
    # each complete top-level form on one line (literal newlines are escaped).
    return '\n'.join(line.replace('\n', ' ') for line in lines) + '\n'


def parse(output, marker, count):
    records, done, blocked = {}, False, False
    prefix = '(' + literal(marker) + ' '
    pattern = re.compile(re.escape(prefix) + r'([0-9]+) (loaded|load-error|not-attempted)\)$')
    for line in output.splitlines():
        if not line.startswith(prefix):
            continue
        if line == prefix + 'done ' + str(count) + ')':
            if done:
                raise ValueError('duplicate completion')
            done = True
            continue
        match = pattern.fullmatch(line)
        if done or not match:
            raise ValueError('invalid ladder record')
        index, status = int(match[1]), match[2]
        if match[1] != str(index) or index != len(records) or index >= count:
            raise ValueError('nonsequential or duplicate ladder record')
        if (blocked and status != 'not-attempted') or (not blocked and status == 'not-attempted'):
            raise ValueError('invalid post-blocker ladder state')
        records[index] = status
        blocked = blocked or status == 'load-error'
    return records, done


def run(files, command, timeout, cwd=None):
    marker = 'RUNE-LADDER-' + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='rune-preload-') as tmp:
        script = Path(tmp) / 'forms.el'
        script.write_text(driver(files, marker), encoding='utf-8')
        args = [sys.executable, '-c', LAUNCHER, str(script), *command, '--eval-stdin']
        output, code, forced = capture(args, cwd, timeout)
    try:
        records, done = parse(output, marker, len(files))
        status = forced or ('completed' if code == 0 and done and len(records) == len(files) else 'incomplete')
    except ValueError as exc:
        records, status = {}, forced or 'protocol-error'
        output += '\n' + str(exc)
    rows = [{'file': row['file'], 'status': records.get(i, 'not-observed')}
            for i, row in enumerate(files)]
    prefix = 0
    for row in rows:
        if row['status'] != 'loaded':
            break
        prefix += 1
    return {'status': status, 'returncode': code, 'loaded_prefix': prefix,
            'first_blocker': next((row['file'] for row in rows if row['status'] == 'load-error'), None),
            'files': rows, 'output': output}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--lisp-root', type=Path, required=True)
    parser.add_argument('--binary', default='./target/release/rune')
    parser.add_argument('--arg', action='append', default=[])
    parser.add_argument('--timeout', type=float, default=60)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--source-revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('--timeout must be finite and positive')
    try:
        files = read_manifest(args.manifest, args.lisp_root)
    except (OSError, ValueError) as exc:
        parser.error(str(exc))
    result = run(files, [args.binary, *args.arg], args.timeout)
    report = {'schema_version': 1, 'measurement': 'explicit-preload-ladder',
              'revision': args.revision, 'source_revision': args.source_revision,
              'limitations': ['Explicit supplied manifest; does not evaluate GNU loadup conditionals.',
                              'Raw-core --eval-stdin bypasses Rune bootstrap; initial load-path contains only source root.',
                              'Load success is not semantic correctness; source provenance is caller supplied.',
                              'File error condition details are not serialized; load-error identifies blocker only.'],
              'run': result}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    return int(result['status'] != 'completed' or result['loaded_prefix'] != len(files))


if __name__ == '__main__':
    raise SystemExit(main())
